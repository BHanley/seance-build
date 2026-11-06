//! AFK chip, top right on Stage and Wall, and beside the ✦ in Chat's list. An
//! optional `afk` script keeps the Mac awake with its lid closed and has leads
//! text you on your phone (Invoke); this chip shows its mark and flips it, the
//! same chip as Invoke's. The script is found at `$SEANCE_AFK_BIN`, else at the
//! path inside `~/.config/seance/afk-bin`. Nothing shows when neither names a
//! script that exists.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{div, prelude::*, px, AnyElement, Context, PromptLevel, SharedString, Window};
use gpui_component::StyledExt as _;

use crate::theme::SeancePalette;

use super::util::tip;
use super::SeanceApp;

/// Width of the chip, so the Stage strip can leave room for it.
pub(super) const AFK_W: f32 = 96.;
/// Render runs far more often than the mark can change.
const RECHECK: Duration = Duration::from_secs(2);
/// An older mark is stale, as in the prompt gate and Invoke.
const STALE_SECS: u64 = 12 * 3600;
/// Its ssh calls time out in seconds; past this, give up so the chip comes back.
const RUN_LIMIT: Duration = Duration::from_secs(60);

struct Seen {
    at: Option<Instant>,
    script: Option<PathBuf>,
    on: bool,
    busy: bool,
}

static SEEN: Mutex<Seen> = Mutex::new(Seen {
    at: None,
    script: None,
    on: false,
    busy: false,
});

fn seen() -> std::sync::MutexGuard<'static, Seen> {
    SEEN.lock().unwrap_or_else(|e| e.into_inner())
}

fn mark_on(home: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(home.join(".local/state/afk/on")) else {
        return false;
    };
    let Ok(since) = text.trim().parse::<u64>() else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    now.saturating_sub(since) <= STALE_SECS
}

/// Where the AFK script is: the env value first, else the path written in
/// `~/.config/seance/afk-bin` (a leading `~/` means the home folder). Only a
/// path that exists counts.
fn find_script(env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    let expand = |raw: &str| -> Option<PathBuf> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        match raw.strip_prefix("~/") {
            Some(rest) => home.map(|h| h.join(rest)),
            None => Some(PathBuf::from(raw)),
        }
    };
    if let Some(p) = env.and_then(expand) {
        return Some(p).filter(|p| p.exists());
    }
    let text = std::fs::read_to_string(home?.join(".config/seance/afk-bin")).ok()?;
    expand(&text).filter(|p| p.exists())
}

/// (script, on, busy), rereading the mark when it's due.
fn state() -> (Option<PathBuf>, bool, bool) {
    let mut s = seen();
    if s.at.is_none_or(|t| t.elapsed() >= RECHECK) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let env = std::env::var("SEANCE_AFK_BIN").ok();
        s.script = find_script(env.as_deref(), home.as_deref());
        s.on = home.as_deref().is_some_and(mark_on);
        s.at = Some(Instant::now());
    }
    (s.script.clone(), s.on, s.busy)
}

impl SeanceApp {
    /// The chip itself, or None where the script isn't installed.
    pub(super) fn render_afk_chip(&self, key: &str, cx: &Context<Self>) -> Option<AnyElement> {
        let (script, on, busy) = state();
        if script.is_none() {
            return None;
        }
        let label = match (busy, on) {
            (true, true) => "Turning off…",
            (true, false) => "Turning on…",
            (false, true) => "✓ AFK ON",
            (false, false) => "AFK",
        };
        Some(
            div()
                .id(SharedString::from(format!("{key}-afk")))
                .flex_none()
                .w(px(AFK_W))
                .flex()
                .justify_center()
                .py(px(2.))
                .rounded_full()
                .border_1()
                .text_xs()
                .when(on, |d| {
                    d.bg(SeancePalette::flame().opacity(0.16))
                        .border_color(gpui::transparent_black())
                        .text_color(SeancePalette::flame())
                        .font_semibold()
                })
                .when(!on, |d| {
                    d.border_color(SeancePalette::border())
                        .text_color(SeancePalette::text_dim())
                })
                .when(!busy, |d| {
                    d.cursor_pointer()
                        .when(!on, |d| d.hover(|s| s.text_color(SeancePalette::text())))
                        .tooltip(tip(if on {
                            "AFK: the Mac stays awake with its lid closed and leads text you in Invoke"
                        } else {
                            "go AFK: keep the Mac awake with its lid closed, leads text you in Invoke"
                        }))
                        .on_click(cx.listener(|this, _, window, cx| this.ask_afk(window, cx)))
                })
                .child(label)
                .into_any_element(),
        )
    }

    /// Confirm, then run `afk on` or `afk off` off the UI thread.
    fn ask_afk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(script), on, false) = state() else {
            return;
        };
        let (msg, detail, yes) = if on {
            (
                "Turn AFK off?",
                "The Mac sleeps with its lid closed again.",
                "Turn off",
            )
        } else {
            (
                "Turn AFK on?",
                "The Mac stays awake with its lid closed and leads text you in Invoke.",
                "Turn on",
            )
        };
        let answer = window.prompt(PromptLevel::Info, msg, Some(detail), &[yes, "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            seen().busy = true;
            let _ = this.update(cx, |_, cx| cx.notify());
            cx.background_executor()
                .spawn(async move {
                    let Ok(mut run) = std::process::Command::new("/bin/bash")
                        .arg(script)
                        .arg(if on { "off" } else { "on" })
                        .spawn()
                    else {
                        return;
                    };
                    let start = Instant::now();
                    while matches!(run.try_wait(), Ok(None)) {
                        if start.elapsed() >= RUN_LIMIT {
                            let _ = run.kill();
                            let _ = run.wait();
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                })
                .await;
            {
                let mut s = seen();
                s.busy = false;
                s.at = None;
            }
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_comes_from_env_then_config_file() {
        let home = std::env::temp_dir().join(format!("seance-afk-test-{}", std::process::id()));
        let cfg = home.join(".config/seance");
        std::fs::create_dir_all(&cfg).unwrap();
        let from_env = home.join("env-afk");
        let from_file = home.join("file-afk");
        std::fs::write(&from_env, "").unwrap();
        std::fs::write(&from_file, "").unwrap();
        // Nothing set: no chip.
        assert_eq!(find_script(None, Some(&home)), None);
        // The config file names a path, `~/` meaning home.
        std::fs::write(cfg.join("afk-bin"), "~/file-afk\n").unwrap();
        assert_eq!(find_script(None, Some(&home)), Some(from_file.clone()));
        // The env wins over the file.
        let env = from_env.to_str().unwrap();
        assert_eq!(find_script(Some(env), Some(&home)), Some(from_env));
        // A path that doesn't exist hides the chip, and the env doesn't fall through.
        assert_eq!(find_script(Some("/no/such/afk"), Some(&home)), None);
        assert_eq!(find_script(Some("  "), Some(&home)), Some(from_file));
        let _ = std::fs::remove_dir_all(&home);
    }
}
