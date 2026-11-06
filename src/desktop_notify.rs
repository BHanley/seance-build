//! Desktop notifications for attention routing (needs-human / ask).
//!
//! Fire-and-forget `notify-send` on Linux. On macOS, `terminal-notifier` when
//! installed (a click brings Seance forward and selects the pane), else
//! `osascript` (a click opens Script Editor, which owns those notifications).
//! Silent no-op if the binary is missing.

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static LAST_MS: AtomicU64 = AtomicU64::new(0);

/// Dedup window so a thrashing agent can't spam the notification daemon.
const MIN_GAP: Duration = Duration::from_secs(4);

pub fn notify(summary: &str, body: &str) {
    post(&LAST_MS, None, summary, body, None);
}

/// Pane alerts (OSC 9/777, finished commands). Own dedup slot, so an alert
/// can't swallow a needs-human or ask notification right behind it.
pub fn notify_alert(pane: &str, summary: &str, body: &str) {
    static LAST_ALERT_MS: AtomicU64 = AtomicU64::new(0);
    post(&LAST_ALERT_MS, Some(pane), summary, body, None);
}

/// A Claude finished working. Dedup is per pane, not global: two Claudes
/// finishing together should both show.
pub fn notify_done(pane: &str, summary: &str, body: &str) {
    static LAST: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
        std::sync::Mutex::new(None);
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    {
        let mut guard = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(Default::default);
        if map
            .get(pane)
            .is_some_and(|prev| wall.saturating_sub(*prev) < MIN_GAP.as_millis() as u64)
        {
            return;
        }
        map.insert(pane.to_string(), wall);
    }
    post(&AtomicU64::new(0), Some(pane), summary, body, None);
}

/// `pane`, when known, is selected on click (macOS with terminal-notifier).
/// `sound` is a macOS sound name for the banner (`default` = the system's);
/// None is silent. Linux banners never carry one.
fn post(last_ms: &AtomicU64, pane: Option<&str>, summary: &str, body: &str, sound: Option<&str>) {
    if !crate::term_config::get().seance_desktop_notifications {
        return;
    }
    let now = Instant::now();
    let now_ms = now.elapsed().as_millis() as u64; // wrong baseline — use wall clock
    let _ = now_ms;
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let prev = last_ms.load(Ordering::Relaxed);
    if wall.saturating_sub(prev) < MIN_GAP.as_millis() as u64 {
        return;
    }
    last_ms.store(wall, Ordering::Relaxed);

    let summary = summary.to_string();
    let body = body.to_string();
    let pane = pane.map(str::to_string);
    let sound = sound.map(str::to_string);
    std::thread::spawn(move || {
        if cfg!(target_os = "macos") {
            if mac_terminal_notifier(pane.as_deref(), &summary, &body, sound.as_deref()) {
                return;
            }
            // Quotes are escaped for the AppleScript string literals.
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            // AppleScript has no "default" sound name; Glass is the closest.
            let with_sound = match sound.as_deref() {
                Some("default") => " sound name \"Glass\"".to_string(),
                Some(name) => format!(" sound name \"{}\"", esc(name)),
                None => String::new(),
            };
            let script = format!(
                "display notification \"{}\" with title \"{}\"{with_sound}",
                esc(&body),
                esc(&summary)
            );
            let _ = Command::new("osascript")
                .args(["-e", &script])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            return;
        }
        let _ = Command::new("notify-send")
            .args([
                "--app-name=seance",
                "--urgency=normal",
                "--expire-time=12000",
                &summary,
                &body,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

/// Apps opened from Finder don't get Homebrew on PATH, so check its
/// prefixes first.
fn terminal_notifier() -> Option<std::path::PathBuf> {
    ["/opt/homebrew/bin", "/usr/local/bin"]
        .iter()
        .map(|d| std::path::Path::new(d).join("terminal-notifier"))
        .find(|p| p.exists())
}

/// Banner groups posted since launch and not yet removed.
static POSTED: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// The human read `pane`: take its banner out of Notification Center.
/// macOS with terminal-notifier only.
pub fn clear_pane(pane: &str) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let group = format!("seance-{pane}");
    // Only groups with a banner up: this runs on every change of pane.
    if !POSTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&group)
    {
        return;
    }
    let Some(bin) = terminal_notifier() else {
        return;
    };
    std::thread::spawn(move || {
        let _ = Command::new(bin)
            .args(["-remove", &group])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

/// False when terminal-notifier isn't installed.
fn mac_terminal_notifier(
    pane: Option<&str>,
    summary: &str,
    body: &str,
    sound: Option<&str>,
) -> bool {
    let Some(bin) = terminal_notifier() else {
        return false;
    };
    let mut cmd = Command::new(bin);
    cmd.args(["-title", summary, "-message", body]);
    // One banner per pane: a new one replaces the last, and reading the pane
    // takes it away (`clear_pane`).
    if let Some(pane) = pane {
        let group = format!("seance-{pane}");
        cmd.args(["-group", &group]);
        POSTED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(group);
    }
    if let Some(sound) = sound {
        cmd.args(["-sound", sound]);
    }
    // A click brings Seance forward (its bundle id from bundle-macos.sh) and
    // selects the pane. No -sender: it disables both, so the icon stays
    // terminal-notifier's.
    cmd.args(["-activate", "xyz.ham.seance"]);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(pane) = pane {
            let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
            let run = format!(
                "{} ctl --all select {}",
                quote(&exe.to_string_lossy()),
                quote(pane)
            );
            cmd.args(["-execute", &run]);
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Play `bell-audio-path` from `~/.config/seance/terminal.conf` when
/// `bell-features` includes `audio`.
/// Fire-and-forget like [`notify`]: `afplay` on macOS, `pw-play` (falling back
/// to `paplay`) on Linux. A burst of bells inside a second plays once.
pub fn bell_sound() {
    static LAST_BELL_MS: AtomicU64 = AtomicU64::new(0);
    let cfg = crate::term_config::get();
    let Some(path) = cfg.bell_audio_path.clone().filter(|_| cfg.bell_audio) else {
        return;
    };
    play(&LAST_BELL_MS, path, cfg.bell_audio_volume);
}

/// `seance-done-sound`: a circle lead finished while you were looking elsewhere.
/// Several finishing within a second play once.
pub fn done_sound() {
    static LAST_DONE_MS: AtomicU64 = AtomicU64::new(0);
    let cfg = crate::term_config::get();
    let Some(path) = cfg.seance_done_sound.clone() else {
        return;
    };
    play(&LAST_DONE_MS, path, cfg.seance_done_sound_volume);
}

/// Play a sound file at `volume` (0 to 1), at most once a second per `last`.
fn play(last: &AtomicU64, path: std::path::PathBuf, volume: f32) {
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if wall.saturating_sub(last.swap(wall, Ordering::Relaxed)) < 1_000 {
        return;
    }
    let volume = volume.to_string();
    std::thread::spawn(move || {
        let quiet = |c: &mut Command| {
            c.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if cfg!(target_os = "macos") {
            quiet(Command::new("afplay").args(["-v", &volume]).arg(&path));
        } else if !quiet(
            Command::new("pw-play")
                .args(["--volume", &volume])
                .arg(&path),
        ) {
            quiet(Command::new("paplay").arg(&path));
        }
    });
}

/// `label` is what the human calls the pane ("circle · name"); `pane` stays
/// the slug the click jumps to.
pub fn needs_human(pane: &str, label: &str, note: Option<&str>) {
    let body = match note {
        Some(n) if !n.is_empty() => format!("{label}: {n}"),
        _ => format!("{label} needs you"),
    };
    let sound = crate::term_config::get().seance_needs_sound.as_deref();
    post(&LAST_MS, Some(pane), "seance · needs human", &body, sound);
}

pub fn ask(from: &str, question: &str) {
    let q = if question.len() > 160 {
        // Cut on a char boundary: a byte slice through an emoji panics.
        let mut end = 160;
        while !question.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &question[..end])
    } else {
        question.to_string()
    };
    let sound = crate::term_config::get().seance_needs_sound.as_deref();
    post(
        &LAST_MS,
        None,
        "seance · agent asks",
        &format!("{from}: {q}"),
        sound,
    );
}
