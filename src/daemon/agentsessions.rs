//! Learn each codex pane's thread id so a crash can put it back
//! (`runtime::engine::agent_session`). claude and grok don't need this: their
//! ids are minted at spawn.

use std::time::Duration;

use crate::runtime::engine::agent_session::{discover_codex_sessions, RolloutCache};

use super::SharedEngine;

/// The window in which a crash loses a freshly started thread.
const EVERY: Duration = Duration::from_secs(10);

pub fn start_agent_session_watcher(engine: SharedEngine) {
    std::thread::Builder::new()
        .name("seance-agent-sessions".into())
        .spawn(move || {
            let mut cache = RolloutCache::new();
            loop {
                std::thread::sleep(EVERY);
                let panes = match engine.lock() {
                    Ok(eng) => eng.codex_pane_pids(),
                    Err(_) => continue,
                };
                // /proc walk happens unlocked.
                let found = discover_codex_sessions(&panes, &mut cache);
                if found.is_empty() {
                    continue;
                }
                let Ok(mut eng) = engine.lock() else { continue };
                if eng.adopt_agent_sessions(&found) {
                    eng.persist();
                }
            }
        })
        .ok();
}
