//! Automatic sleep: Claude panes nobody has touched in a while stop costing
//! RAM (12h by default; `seance-auto-sleep-idle` in terminal.conf).
//!
//! Deliberately conservative. It only sleeps a pane that can be woken back
//! exactly (`Engine::pane_restorable`: an agent pane whose conversation is on
//! disk, or Claude hosted in a shell with a session record), never one that
//! is busy, waiting on the human, holding queued tasks or focused, and never
//! in a circle listed in `seance-auto-sleep-skip`. A `claude-mac` pane with a
//! pinned conversation id is restorable too, but only by hand: the sweep can't
//! see what the Mac side is doing (a background server, a watcher), so it
//! leaves those up. Idle is human input and
//! the pane's busy changes, not screen output, which a Claude status line
//! never stops making. Anything addressed to a sleeping pane wakes it.
//!
//! Manual sleep (`seance ctl sleep`, right-click → sleep) still sleeps whole
//! circles, `claude-mac` panes with a pinned id included.

use std::time::Duration;

use crate::runtime::engine::auto_sleep_idle_ms;

use super::SharedEngine;

/// How often to look. Idleness is measured in hours — checking every few
/// minutes is already far finer-grained than the threshold.
const SWEEP: Duration = Duration::from_secs(300);

/// Start the sweep with terminal.conf's threshold and skip list; nothing
/// when `seance-auto-sleep-idle = off`.
pub fn start_sleep_sweeper(engine: SharedEngine) {
    let idle_ms = auto_sleep_idle_ms();
    if idle_ms == 0 {
        return;
    }
    let skip = crate::term_config::get().seance_auto_sleep_skip.clone();
    start_sleep_sweeper_with(engine, idle_ms, skip, SWEEP)
}

pub fn start_sleep_sweeper_with(
    engine: SharedEngine,
    idle_ms: u64,
    skip: Vec<String>,
    every: Duration,
) {
    std::thread::Builder::new()
        .name("seance-sleep-sweep".into())
        .spawn(move || loop {
            std::thread::sleep(every);
            let Ok(mut eng) = engine.lock() else { continue };
            let slept = eng.auto_sleep_sweep(idle_ms, &skip);
            if !slept.is_empty() {
                eprintln!(
                    "[seance daemon] auto-slept idle panes: {}",
                    slept.join(", ")
                );
                eng.persist();
                eng.push_state_to_all();
            }
        })
        .ok();
}
