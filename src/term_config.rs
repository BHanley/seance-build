//! Terminal settings from `~/.config/seance/terminal.conf`.
//!
//! The file uses Ghostty's config syntax so a Ghostty user can name a theme
//! or include their existing config (`config-file = ~/.config/ghostty/config`)
//! instead of copying values over. Only the keys below are read; everything
//! else is ignored. No file means the built-in look, unchanged.
//!
//! Read once per process. The daemon resolves cell colors from the palette;
//! the GUI uses the default fg/bg, the font, the bell sound and the
//! command-finish rule. Each machine reads its own file, so a thin client and
//! its daemon each need one.
//!
//! | key | used by |
//! |-----|---------|
//! | `theme` | Ghostty theme name or path (palette/fg/bg/cursor) |
//! | `palette`, `background`, `foreground`, `cursor-color` | colors, override the theme |
//! | `font-family`, `font-size` (points, like Ghostty) | GUI font |
//! | `bell-features` (`audio`), `bell-audio-path`, `bell-audio-volume` | sound on BEL |
//! | `notify-on-command-finish` (`never`/`unfocused`/`always`), `notify-on-command-finish-after` | shell command alerts |
//! | `config-file` | include another file (`?` prefix = optional) |
//! | `seance-theme` | startup color scheme until one is picked (daemon; see `looks`) |
//! | `seance-circle-map` | TSV of `circle-slug<TAB>folder` for Stage's lead log line (GUI) |
//! | `seance-health-file` | health report Stage's footer reads (GUI) |
//! | `seance-usage-file` | Claude plan usage JSON (`five_hour`/`seven_day`: `used`, `resets_at`) for the footer's usage bars (GUI) |
//! | `seance-needs-sound` | macOS sound name for needs-you banners (`default` = the system's) |
//! | `seance-auto-sleep-idle` (e.g. `6h`), `seance-auto-sleep-skip` (circle slugs, comma separated) | daemon's auto-sleep of idle Claude panes |
//! | `seance-done-sound`, `seance-done-sound-volume` | sound when a circle lead finishes out of sight |
//! | `seance-lead-sprite.<circle-slug>` | a seed that swaps that circle's lead creature (GUI) |
//! | `seance-new-circle-command` | the first pane of a circle made with Stage's "+ circle" (GUI; runs on the daemon's machine) |
//! | `seance-desktop-notifications` (`true`/`false`, also `off`/`0`) | `false` posts no macOS/Linux banners at all; sounds and clearing stale banners are unaffected (read at launch) |
//! | `seance-origin-gutter` (`true`/`false`, also `off`/`0`) | the 2px input-origin stripe at a terminal's left edge (GUI) |
//!
//! The two Stage paths are read through the daemon's fs bridge, so they name
//! files on the daemon's machine and a leading `~` is the daemon's home.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The palette seance shipped with (base16 default), kept as the fallback.
const DEFAULT_PALETTE: [u32; 16] = [
    0x00_18_18_18, //  0 black
    0x00_ab_46_42, //  1 red
    0x00_a1_b5_6c, //  2 green
    0x00_f7_ca_88, //  3 yellow
    0x00_7c_af_c2, //  4 blue
    0x00_ba_8b_af, //  5 magenta
    0x00_86_c1_b9, //  6 cyan
    0x00_d8_d8_d8, //  7 white
    0x00_58_58_58, //  8 bright black
    0x00_ab_46_42, //  9 bright red
    0x00_a1_b5_6c, // 10 bright green
    0x00_f7_ca_88, // 11 bright yellow
    0x00_7c_af_c2, // 12 bright blue
    0x00_ba_8b_af, // 13 bright magenta
    0x00_86_c1_b9, // 14 bright cyan
    0x00_f8_f8_f8, // 15 bright white
];

/// Include depth cap, so a config-file loop can't recurse forever.
const MAX_INCLUDE_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandFinishNotify {
    Never,
    Unfocused,
    Always,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TermConfig {
    pub palette: [u32; 16],
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub font_family: Option<String>,
    /// Points, as Ghostty means them. See [`crate::term_font::font_size`].
    pub font_size: Option<f32>,
    /// Only played when `bell-features` includes `audio`, as in Ghostty.
    pub bell_audio_path: Option<PathBuf>,
    pub bell_audio: bool,
    pub bell_audio_volume: f32,
    pub notify_on_command_finish: CommandFinishNotify,
    pub notify_on_command_finish_after_ms: u64,
    /// `seance-theme`: the scheme a daemon with no saved choice starts on.
    /// Not Ghostty's; Ghostty ignores keys it doesn't know.
    pub seance_theme: Option<String>,
    /// `seance-circle-map`, kept unexpanded: the daemon resolves `~`.
    pub seance_circle_map: Option<String>,
    /// `seance-health-file`, kept unexpanded like the circle map.
    pub seance_health_file: Option<String>,
    /// `seance-usage-file`, kept unexpanded like the health file.
    pub seance_usage_file: Option<String>,
    /// `seance-needs-sound`: the macOS sound a needs-you banner plays. None
    /// keeps banners silent, as before.
    pub seance_needs_sound: Option<String>,
    /// `seance-done-sound`: played when a pane finishes while you're looking
    /// elsewhere (its done mark). None plays nothing.
    pub seance_done_sound: Option<PathBuf>,
    pub seance_done_sound_volume: f32,
    /// `seance-auto-sleep-idle`: how long a Claude pane sits idle before the
    /// daemon sleeps it. None keeps the built-in default.
    pub seance_auto_sleep_idle_ms: Option<u64>,
    /// `seance-auto-sleep-skip`: circles whose panes are never auto-slept.
    pub seance_auto_sleep_skip: Vec<String>,
    /// `seance-new-circle-command`: what the first pane of a circle made with
    /// Stage's "+ circle" runs. None is a plain shell.
    pub seance_new_circle_command: Option<String>,
    /// `seance-desktop-notifications = false` posts no banners.
    pub seance_desktop_notifications: bool,
    /// `seance-origin-gutter = false` hides the input-origin stripe.
    pub seance_origin_gutter: bool,
    /// `seance-lead-sprite.<circle-slug> = SEED`: re-seeds that circle's creature.
    pub lead_sprites: std::collections::HashMap<String, String>,
}

impl Default for TermConfig {
    fn default() -> Self {
        Self {
            palette: DEFAULT_PALETTE,
            foreground: 0x00_d8_d8_d8,
            background: 0x00_18_18_18,
            cursor: 0x00_e5_c0_7b,
            font_family: None,
            font_size: None,
            bell_audio_path: None,
            bell_audio: false,
            bell_audio_volume: 1.0,
            notify_on_command_finish: CommandFinishNotify::Never,
            notify_on_command_finish_after_ms: 5_000,
            seance_theme: None,
            seance_circle_map: None,
            seance_health_file: None,
            seance_usage_file: None,
            seance_needs_sound: None,
            seance_done_sound: None,
            seance_done_sound_volume: 0.6,
            seance_auto_sleep_idle_ms: None,
            seance_auto_sleep_skip: Vec::new(),
            seance_new_circle_command: None,
            seance_desktop_notifications: true,
            seance_origin_gutter: true,
            lead_sprites: Default::default(),
        }
    }
}

pub fn get() -> &'static TermConfig {
    static CONFIG: OnceLock<TermConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let path = PathBuf::from(shellexpand::tilde("~/.config/seance/terminal.conf").as_ref());
        load(&path)
    })
}

fn load(path: &Path) -> TermConfig {
    let mut entries = Vec::new();
    collect_entries(path, 0, &mut entries);
    let mut cfg = TermConfig::default();
    // Ghostty applies the theme first, so explicit colors in the config win.
    if let Some((_, theme)) = entries.iter().rev().find(|(k, _)| k == "theme") {
        // `light:X,dark:Y` picks dark: the terminal ground is always dark.
        let theme = theme
            .split(',')
            .find_map(|t| t.trim().strip_prefix("dark:"))
            .unwrap_or(theme);
        if let Some(theme_path) = find_theme(theme) {
            let mut theme_entries = Vec::new();
            collect_entries(&theme_path, MAX_INCLUDE_DEPTH, &mut theme_entries);
            for (k, v) in &theme_entries {
                apply(&mut cfg, k, v);
            }
        }
    }
    for (k, v) in &entries {
        apply(&mut cfg, k, v);
    }
    cfg
}

/// `key = value` pairs from `path` in order, with `config-file` includes
/// appended after the file's own lines (Ghostty's include order).
fn collect_entries(path: &Path, depth: usize, out: &mut Vec<(String, String)>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let base = path.parent().unwrap_or(Path::new("."));
    let mut includes = Vec::new();
    for (key, value) in parse_lines(&text) {
        match key.as_str() {
            "config-file" => includes.push(value),
            // Relative paths mean "next to this file", as in Ghostty.
            "bell-audio-path" | "seance-done-sound" if !value.is_empty() => {
                let p = PathBuf::from(shellexpand::tilde(&value).as_ref());
                out.push((key, base.join(p).to_string_lossy().into_owned()));
            }
            _ => out.push((key, value)),
        }
    }
    if depth >= MAX_INCLUDE_DEPTH {
        return;
    }
    for inc in includes {
        let inc = unquote(inc.trim_start_matches('?'));
        let inc = PathBuf::from(shellexpand::tilde(inc).as_ref());
        collect_entries(&base.join(inc), depth + 1, out);
    }
}

fn parse_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), unquote(v.trim()).to_string()))
        })
        .collect()
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

fn apply(cfg: &mut TermConfig, key: &str, value: &str) {
    match key {
        "palette" => {
            if let Some((idx, color)) = value.split_once('=') {
                if let (Ok(idx), Some(color)) = (idx.trim().parse::<usize>(), parse_color(color)) {
                    if idx < 16 {
                        cfg.palette[idx] = color;
                    }
                }
            }
        }
        "background" => set_color(&mut cfg.background, value),
        "foreground" => set_color(&mut cfg.foreground, value),
        "cursor-color" => set_color(&mut cfg.cursor, value),
        // Ghostty: the first font-family is the font, later ones are
        // fallbacks, and an empty value clears the list.
        "font-family" if value.is_empty() => cfg.font_family = None,
        "font-family" if cfg.font_family.is_none() => cfg.font_family = Some(value.to_string()),
        "font-size" => {
            if let Some(size) = value.parse().ok().filter(|s: &f32| *s > 0.0) {
                cfg.font_size = Some(size);
            }
        }
        "bell-audio-path" if !value.is_empty() => cfg.bell_audio_path = Some(PathBuf::from(value)),
        "bell-features" => {
            for feature in value.split(',').map(str::trim) {
                match feature {
                    "audio" => cfg.bell_audio = true,
                    "no-audio" => cfg.bell_audio = false,
                    _ => {}
                }
            }
        }
        "bell-audio-volume" => {
            if let Ok(v) = value.parse::<f32>() {
                cfg.bell_audio_volume = v.clamp(0.0, 1.0);
            }
        }
        "notify-on-command-finish" => {
            cfg.notify_on_command_finish = match value {
                "unfocused" => CommandFinishNotify::Unfocused,
                "always" => CommandFinishNotify::Always,
                _ => CommandFinishNotify::Never,
            }
        }
        "notify-on-command-finish-after" => {
            if let Some(ms) = parse_duration_ms(value) {
                cfg.notify_on_command_finish_after_ms = ms;
            }
        }
        "seance-theme" if !value.is_empty() => cfg.seance_theme = Some(value.to_string()),
        "seance-circle-map" => cfg.seance_circle_map = non_empty(value),
        "seance-health-file" => cfg.seance_health_file = non_empty(value),
        "seance-usage-file" => cfg.seance_usage_file = non_empty(value),
        "seance-needs-sound" => cfg.seance_needs_sound = non_empty(value),
        "seance-new-circle-command" => cfg.seance_new_circle_command = non_empty(value),
        "seance-desktop-notifications" => {
            cfg.seance_desktop_notifications = !matches!(value, "false" | "off" | "0")
        }
        "seance-origin-gutter" => {
            cfg.seance_origin_gutter = !matches!(value, "false" | "off" | "0")
        }
        "seance-done-sound" => cfg.seance_done_sound = non_empty(value).map(PathBuf::from),
        // `off` (or 0) turns auto-sleep off.
        "seance-auto-sleep-idle" => {
            cfg.seance_auto_sleep_idle_ms = if value == "off" || value == "0" {
                Some(0)
            } else {
                parse_duration_ms(value)
            }
        }
        "seance-auto-sleep-skip" => {
            cfg.seance_auto_sleep_skip = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        "seance-done-sound-volume" => {
            if let Ok(v) = value.parse::<f32>() {
                cfg.seance_done_sound_volume = v.clamp(0.0, 1.0);
            }
        }
        _ => {
            if let Some(circle) = key
                .strip_prefix("seance-lead-sprite.")
                .filter(|c| !c.is_empty())
            {
                match non_empty(value) {
                    Some(seed) => cfg.lead_sprites.insert(circle.to_string(), seed),
                    None => cfg.lead_sprites.remove(circle),
                };
            }
        }
    }
}

/// An empty value clears the key, as in Ghostty.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn set_color(slot: &mut u32, value: &str) {
    if let Some(c) = parse_color(value) {
        *slot = c;
    }
}

/// `#rrggbb` or `rrggbb`. Ghostty also takes X11 color names; those are skipped.
fn parse_color(value: &str) -> Option<u32> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// Ghostty durations like `5s`, `500ms`, `1m`, or `1m30s`.
fn parse_duration_ms(value: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut rest = value.trim();
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit())?;
        let n: u64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        let mult = match &rest[..unit_len] {
            "ms" => 1,
            "s" => 1_000,
            "m" => 60_000,
            "h" => 3_600_000,
            _ => return None,
        };
        total += n * mult;
        rest = &rest[unit_len..];
    }
    Some(total)
}

/// A theme name resolves against Ghostty's theme folders; a path is used as-is.
fn find_theme(theme: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(shellexpand::tilde(theme).as_ref());
    if direct.is_absolute() {
        return direct.is_file().then_some(direct);
    }
    let mut dirs = vec![PathBuf::from(
        shellexpand::tilde("~/.config/ghostty/themes").as_ref(),
    )];
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(d).join("ghostty/themes")),
    );
    dirs.push(PathBuf::from(
        "/Applications/Ghostty.app/Contents/Resources/ghostty/themes",
    ));
    dirs.into_iter()
        .map(|d| d.join(theme))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("seance-term-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn missing_file_is_the_default_look() {
        assert_eq!(
            load(Path::new("/nonexistent/terminal.conf")),
            TermConfig::default()
        );
    }

    #[test]
    fn reads_colors_font_bell_and_notify() {
        let dir = test_dir("basic");
        let p = write(
            &dir,
            "terminal.conf",
            "# comment\n\
             palette = 1=#ff6188\n\
             background = 2d2a2e\n\
             foreground = \"#fcfcfa\"\n\
             font-size = 14\n\
             bell-features = attention,audio\n\
             bell-audio-path = sounds/hero.wav\n\
             bell-audio-volume = 0.8\n\
             notify-on-command-finish = unfocused\n\
             notify-on-command-finish-after = 1m30s\n",
        );
        let cfg = load(&p);
        assert_eq!(cfg.palette[1], 0xff6188);
        assert_eq!(cfg.palette[0], DEFAULT_PALETTE[0]);
        assert_eq!(cfg.background, 0x2d2a2e);
        assert_eq!(cfg.foreground, 0xfcfcfa);
        assert_eq!(cfg.font_size, Some(14.0));
        assert!(cfg.bell_audio);
        assert_eq!(cfg.bell_audio_path, Some(dir.join("sounds/hero.wav")));
        assert_eq!(cfg.bell_audio_volume, 0.8);
        assert_eq!(cfg.notify_on_command_finish, CommandFinishNotify::Unfocused);
        assert_eq!(cfg.notify_on_command_finish_after_ms, 90_000);
        assert_eq!(cfg.seance_needs_sound, None);
        assert_eq!(cfg.seance_done_sound, None);
    }

    #[test]
    fn reads_lead_sprite_seeds() {
        let dir = test_dir("sprites");
        let p = write(
            &dir,
            "terminal.conf",
            "seance-lead-sprite.alpha-circle = frog\nseance-lead-sprite.main = x\nseance-lead-sprite.main =\nseance-lead-sprite. = y\n",
        );
        let cfg = load(&p);
        assert_eq!(
            cfg.lead_sprites.get("alpha-circle").map(String::as_str),
            Some("frog")
        );
        assert_eq!(cfg.lead_sprites.len(), 1);
    }

    #[test]
    fn reads_new_circle_command() {
        let dir = test_dir("newcircle");
        let p = write(
            &dir,
            "terminal.conf",
            "seance-new-circle-command = claude-mac\n",
        );
        assert_eq!(
            load(&p).seance_new_circle_command.as_deref(),
            Some("claude-mac")
        );
        assert_eq!(TermConfig::default().seance_new_circle_command, None);
    }

    #[test]
    fn a_leftover_hud_key_is_ignored() {
        let dir = test_dir("hud-gone");
        let p = write(
            &dir,
            "terminal.conf",
            "seance-hud = ink\nseance-desktop-notifications = false\n",
        );
        // The key is no longer read, and the rest of the file still is.
        assert!(!load(&p).seance_desktop_notifications);
    }

    #[test]
    fn reads_desktop_notifications() {
        let dir = test_dir("banners");
        let p = write(
            &dir,
            "terminal.conf",
            "seance-desktop-notifications = false\n",
        );
        assert!(!load(&p).seance_desktop_notifications);
        let p = write(
            &dir,
            "terminal.conf",
            "seance-desktop-notifications = off\n",
        );
        assert!(!load(&p).seance_desktop_notifications);
        let p = write(
            &dir,
            "terminal.conf",
            "seance-desktop-notifications = true\n",
        );
        assert!(load(&p).seance_desktop_notifications);
        assert!(TermConfig::default().seance_desktop_notifications);
    }

    #[test]
    fn reads_origin_gutter() {
        let dir = test_dir("gutter");
        let p = write(&dir, "terminal.conf", "seance-origin-gutter = false\n");
        assert!(!load(&p).seance_origin_gutter);
        let p = write(&dir, "terminal.conf", "seance-origin-gutter = true\n");
        assert!(load(&p).seance_origin_gutter);
        let p = write(&dir, "terminal.conf", "seance-origin-gutter = off\n");
        assert!(!load(&p).seance_origin_gutter);
        assert!(TermConfig::default().seance_origin_gutter);
    }

    #[test]
    fn reads_needs_and_done_sounds() {
        let dir = test_dir("sounds");
        let p = write(
            &dir,
            "terminal.conf",
            "seance-needs-sound = default\n\
             seance-done-sound = sounds/ding.aiff\n\
             seance-done-sound-volume = 0.6\n",
        );
        let cfg = load(&p);
        assert_eq!(cfg.seance_needs_sound.as_deref(), Some("default"));
        assert_eq!(cfg.seance_done_sound, Some(dir.join("sounds/ding.aiff")));
        assert_eq!(cfg.seance_done_sound_volume, 0.6);
    }

    #[test]
    fn reads_auto_sleep_settings() {
        let dir = test_dir("autosleep");
        let p = write(
            &dir,
            "terminal.conf",
            "seance-auto-sleep-idle = 6h\n\
             seance-auto-sleep-skip = alpha-circle, main\n",
        );
        let cfg = load(&p);
        assert_eq!(cfg.seance_auto_sleep_idle_ms, Some(6 * 3_600_000));
        assert_eq!(cfg.seance_auto_sleep_skip, vec!["alpha-circle", "main"]);
        assert_eq!(TermConfig::default().seance_auto_sleep_idle_ms, None);
        let off = write(&dir, "off.conf", "seance-auto-sleep-idle = off\n");
        assert_eq!(load(&off).seance_auto_sleep_idle_ms, Some(0));
    }

    #[test]
    fn includes_load_after_the_file_and_explicit_colors_beat_the_theme() {
        let dir = test_dir("include");
        let theme = write(
            &dir,
            "mytheme",
            "background = 111111\nforeground = 222222\n",
        );
        write(
            &dir,
            "ghostty",
            "font-size = 9\nfont-size = big\nforeground = 333333\nfont-family = Main\nfont-family = Fallback\n",
        );
        let p = write(
            &dir,
            "terminal.conf",
            &format!(
                "config-file = ?\"ghostty\"\nconfig-file = ?missing\nfont-size = 12\ntheme = light:nope,dark:{}\n",
                theme.display()
            ),
        );
        let cfg = load(&p);
        // The include comes after the file's own lines, so its font-size wins.
        assert_eq!(cfg.font_size, Some(9.0));
        assert_eq!(cfg.font_family.as_deref(), Some("Main"));
        assert_eq!(cfg.background, 0x111111);
        assert_eq!(cfg.foreground, 0x333333);
    }

    #[test]
    fn seance_theme_key() {
        let dir = test_dir("scheme");
        let p = write(&dir, "terminal.conf", "seance-theme = \"Ouija\"\n");
        assert_eq!(load(&p).seance_theme.as_deref(), Some("Ouija"));
        let p = write(&dir, "terminal.conf", "seance-theme =\n");
        assert_eq!(load(&p).seance_theme, None);
    }

    #[test]
    fn stage_paths_stay_unexpanded_for_the_daemon() {
        let dir = test_dir("stage");
        let p = write(
            &dir,
            "terminal.conf",
            "seance-circle-map = ~/projects/circles.tsv\nseance-health-file = \"~/.local/state/seance-health/latest.md\"\n",
        );
        let cfg = load(&p);
        // `~` must reach the daemon as-is: a Mac GUI expanding it would ask
        // the daemon host for /Users/... .
        assert_eq!(
            cfg.seance_circle_map.as_deref(),
            Some("~/projects/circles.tsv")
        );
        assert_eq!(
            cfg.seance_health_file.as_deref(),
            Some("~/.local/state/seance-health/latest.md")
        );
        let p = write(&dir, "terminal.conf", "seance-circle-map =\n");
        assert_eq!(load(&p).seance_circle_map, None);
        assert_eq!(load(&p).seance_health_file, None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms("5s"), Some(5_000));
        assert_eq!(parse_duration_ms("250ms"), Some(250));
        assert_eq!(parse_duration_ms("five"), None);
    }
}
