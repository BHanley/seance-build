//! The daemon's color scheme choice: which `seance_core::schemes` entry is
//! active, saved in the state dir so it survives restart and upgrade.
//!
//! The daemon owns it because terminal cells are resolved here and every GUI
//! (local, or a thin client over the tunnel) has to agree. GUIs learn it from
//! `GuiEvent::Theme`; `theme.rs` is their side. Picking order at boot: the
//! saved choice, then `seance-theme` in terminal.conf, then candlelit.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use seance_core::schemes::{self, Scheme, TermColors};

use crate::runtime::pty_session::{set_term_colors, term_colors};

/// Index into `schemes::SCHEMES`.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn active() -> &'static Scheme {
    &schemes::SCHEMES[ACTIVE.load(Ordering::Relaxed)]
}

fn path() -> PathBuf {
    crate::runtime::state_data_dir().join("theme.json")
}

/// `{"scheme":"<id>"}`: an object, so later looks settings can sit beside it.
fn encode(id: &str) -> String {
    serde_json::json!({ "scheme": id }).to_string()
}

/// The saved scheme, or `None` for a missing key, garbage, or an id this
/// build doesn't know.
fn decode(body: &str) -> Option<&'static Scheme> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    schemes::find(v.get("scheme")?.as_str()?)
}

fn startup(saved: Option<&str>, config: Option<&str>) -> &'static Scheme {
    saved
        .and_then(decode)
        .or_else(|| config.and_then(schemes::find))
        .unwrap_or(&schemes::SCHEMES[0])
}

/// Make `s` active for this process. Returns the terminal colors before.
fn activate(s: &'static Scheme) -> TermColors {
    let idx = schemes::index_of(s.id).unwrap_or(0);
    ACTIVE.store(idx, Ordering::Relaxed);
    set_term_colors(s.term)
}

/// Daemon boot. Runs before any pane is spawned or adopted, so the first
/// palette seed is already the right one.
pub fn boot() {
    let saved = std::fs::read_to_string(path()).ok();
    let config = crate::term_config::get().seance_theme.as_deref();
    if let Some(id) = config.filter(|id| schemes::find(id).is_none()) {
        eprintln!(
            "[seance daemon] seance-theme '{id}' is not a theme ({}); using the default",
            schemes::id_list()
        );
    }
    activate(startup(saved.as_deref(), config));
}

/// Switch and save. Returns the scheme and the terminal colors before and
/// after, so live panes can be reseeded.
pub fn set(id: &str) -> Result<(&'static Scheme, TermColors, TermColors), String> {
    let s = schemes::find(id).ok_or_else(|| {
        format!(
            "unknown theme '{}'; one of: {}",
            id.trim(),
            schemes::id_list()
        )
    })?;
    // A failed save still switches: the look is live, it just won't survive
    // a restart, which is worth a log line and not a refusal.
    if let Err(e) = save(&encode(s.id)) {
        eprintln!("[seance daemon] theme save: {e}");
    }
    let old = activate(s);
    Ok((s, old, term_colors()))
}

fn save(body: &str) -> std::io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("seance-tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_roundtrips_and_rejects_junk() {
        assert_eq!(decode(&encode("tarot")).map(|s| s.id), Some("tarot"));
        assert!(decode("").is_none());
        assert!(decode("{\"scheme\":\"plaid\"}").is_none());
        assert!(decode("{\"scheme\":3}").is_none());
        assert!(decode("[]").is_none());
    }

    #[test]
    fn saved_choice_beats_config_and_unknowns_fall_back() {
        let saved = encode("riso");
        assert_eq!(startup(Some(&saved), Some("synth")).id, "riso");
        assert_eq!(startup(None, Some("synth")).id, "synth");
        assert_eq!(startup(Some("junk"), Some("Synth")).id, "synth");
        assert_eq!(startup(None, Some("plaid")).id, schemes::DEFAULT);
        assert_eq!(startup(None, None).id, schemes::DEFAULT);
    }
}
