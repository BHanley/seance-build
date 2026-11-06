//! Host circle-mode poller: for every host.json `circle_modes[]` entry with a
//! `state_cmd`, run it on its clock and take its answer as the truth about
//! which circles are in the mode (`Engine::ingest_mode_state`).
//!
//! The host owns the state because seance can't see most of the ways in and
//! out of it (vita's AFK is entered from the phone, expires on a TTL, ends
//! with "release" in telegram). seance's own menu toggle only holds the badge
//! until a poll agrees; while one is pending the mode polls every
//! [`PENDING_POLL`] so the confirmation lands quickly.
//!
//! The command runs with the engine UNLOCKED (vita's takes ~1–2s and itself
//! calls `seance ctl list`, which needs the engine) — only the ingest locks.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::SharedEngine;

const TICK: Duration = Duration::from_secs(2);
const DEFAULT_POLL: Duration = Duration::from_secs(30);
const PENDING_POLL: Duration = Duration::from_secs(10);

/// Start the poll thread. Idle (one config re-read per tick) when no mode
/// has a `state_cmd`.
pub fn start_mode_poller(engine: SharedEngine) {
    std::thread::Builder::new()
        .name("seance-mode-poll".into())
        .spawn(move || {
            let mut last_run: HashMap<String, Instant> = HashMap::new();
            loop {
                for mode in crate::host::circle_modes() {
                    let Some(cmd) = mode.state_cmd.as_deref() else {
                        continue;
                    };
                    let pending = engine
                        .lock()
                        .map(|e| e.mode_pending(&mode.id))
                        .unwrap_or(false);
                    let every = if pending {
                        PENDING_POLL
                    } else {
                        mode.poll_secs
                            .map(Duration::from_secs)
                            .unwrap_or(DEFAULT_POLL)
                    };
                    if last_run.get(&mode.id).is_some_and(|t| t.elapsed() < every) {
                        continue;
                    }
                    last_run.insert(mode.id.clone(), Instant::now());
                    let field = mode.state_field.as_deref().unwrap_or("on");
                    let parsed = crate::host::run_state_cmd(cmd)
                        .and_then(|out| crate::host::parse_mode_state(&out, field));
                    match parsed {
                        Ok((circles, panes)) => {
                            if let Ok(mut eng) = engine.lock() {
                                let now = crate::runtime::engine::helpers::now_ms();
                                if eng.ingest_mode_state(&mode.id, &circles, &panes, now) {
                                    eng.persist();
                                    eng.push_state_to_all();
                                }
                            }
                        }
                        // Keep the last known state: a broken command must
                        // not clear everyone's badge.
                        Err(e) => eprintln!("[seance] circle mode {}: state_cmd: {e}", mode.id),
                    }
                }
                std::thread::sleep(TICK);
            }
        })
        .ok();
}
