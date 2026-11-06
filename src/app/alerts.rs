//! Pane alerts (BEL, OSC 9 / OSC 777, long shell commands) turned into what a
//! terminal like Ghostty gives you: the bell sound, and a desktop notification
//! when you aren't looking at that pane. Sound and the command rule come from
//! `~/.config/seance/terminal.conf` (see `term_config`).

use std::time::Duration;

use gpui::Context;

use super::util::now_ms;
use super::workspaces::WorkspaceAttention;
use super::SeanceApp;
use crate::desktop_notify;
use crate::term_config::CommandFinishNotify;

/// How long Stage's "Snooze 1h" quiets a pane.
const SNOOZE_MS: u64 = 60 * 60 * 1000;

/// Stage's snoozed panes: slug to until (unix ms). While one is on, the
/// pane's Stage chip, banners and dings are quiet; its row and status stay.
/// It ends when the hour is up or the human opens the pane.
#[derive(Default)]
pub(super) struct Snoozes(pub(super) std::collections::HashMap<String, u64>);

impl Snoozes {
    fn add(&mut self, slug: &str, now: u64) {
        self.0.insert(slug.to_string(), now + SNOOZE_MS);
    }

    pub(super) fn on(&self, slug: &str, now: u64) -> bool {
        self.0.get(slug).is_some_and(|until| *until > now)
    }

    /// Ended early (the pane was opened). True when one was on.
    fn clear(&mut self, slug: &str, now: u64) -> bool {
        let was = self.on(slug, now);
        self.0.remove(slug);
        was
    }

    fn prune(&mut self, now: u64) {
        self.0.retain(|_, until| *until > now);
    }
}

impl SeanceApp {
    pub(super) fn on_pane_alert(
        &self,
        pane: &str,
        kind: &str,
        title: &str,
        body: &str,
        duration_ms: u64,
        exit_code: Option<i32>,
    ) {
        // Snoozed on Stage: no banner, but a bell is the terminal's own.
        if self.snoozed(pane) && kind != "bell" {
            return;
        }
        let cfg = crate::term_config::get();
        // Looking right at the pane: like a focused terminal, sound only.
        let watching = self.window_active && self.active_slug.as_deref() == Some(pane);
        let name = self.pane_label(pane);
        match kind {
            // Sound only, as in Ghostty: shells ring on every failed tab
            // completion, and a program that wants a banner sends OSC 9/777.
            "bell" => desktop_notify::bell_sound(),
            "done" if !watching => {
                desktop_notify::notify_done(pane, &format!("{name}: done"), body);
            }
            "notify" if !watching => {
                let summary = if title.is_empty() {
                    name
                } else {
                    format!("{name}: {title}")
                };
                desktop_notify::notify_alert(pane, &summary, body);
            }
            "command" => {
                let wanted = match cfg.notify_on_command_finish {
                    CommandFinishNotify::Never => false,
                    CommandFinishNotify::Unfocused => !watching,
                    CommandFinishNotify::Always => true,
                };
                if wanted && duration_ms >= cfg.notify_on_command_finish_after_ms {
                    let status = match exit_code {
                        Some(0) => "Done".to_string(),
                        Some(code) => format!("Failed ({code})"),
                        None => "Finished".to_string(),
                    };
                    desktop_notify::bell_sound();
                    desktop_notify::notify_alert(pane, &format!("{name}: {status}"), body);
                }
            }
            _ => {}
        }
    }

    /// Mark a pane unread (an "Unread" chip and dot until you next open it),
    /// or clear the mark. Marking the pane you're on keeps it marked until
    /// you leave and come back.
    pub(super) fn mark_unread(&mut self, slug: &str, on: bool) {
        let changed = if on {
            self.subs_pref.later.insert(slug.to_string())
        } else {
            self.subs_pref.later.remove(slug)
        };
        if changed {
            self.save_arrangement();
        }
    }

    /// Opening a pane marked unread reads it, takes its banner out of
    /// Notification Center and ends its snooze. Only a change of pane
    /// counts, and only in the window you're using. The Wall only shows
    /// circles, so moving across its tiles opens nothing; Stage or Classic
    /// count the pane they show when you get there.
    pub(super) fn read_later(&mut self) {
        if matches!(
            self.view.mode,
            super::views::ViewMode::Wall | super::views::ViewMode::Chat
        ) || self.active_slug == self.later_last
        {
            return;
        }
        self.later_last = self.active_slug.clone();
        if !self.window_active {
            return;
        }
        if let Some(slug) = self.active_slug.clone() {
            let slug = slug.as_str();
            desktop_notify::clear_pane(slug);
            if self.snoozes.clear(slug, now_ms()) {
                self.save_snoozes();
            }
            if self.subs_pref.later.remove(slug) {
                self.save_arrangement();
            }
        }
    }

    /// A Claude pane went idle and stayed idle: its ✓, and the done banner
    /// when `loud` (the turn's seconds). Not when it was Claude Code
    /// compacting an idle session, which wears the spinner like a turn: the
    /// screen says so. A circle that isn't streaming here has a stale
    /// screen, so ask for a fresh one and look a moment later.
    pub(super) fn pane_finished(&mut self, pane: &str, loud: Option<u64>, cx: &mut Context<Self>) {
        let Some(ws) = self
            .panes
            .iter()
            .find(|p| p.slug == pane)
            .map(|p| &p.workspace)
        else {
            return;
        };
        // The Wall streams the subscribed circles, the other views the one on
        // show.
        let streamed = if self.overview() {
            self.subscriptions.contains(ws)
        } else {
            self.selected_workspace.as_ref() == Some(ws)
        };
        if streamed {
            self.turn_done(pane, loud, cx);
            return;
        }
        if self.screen_check.insert(pane.to_string(), loud).is_some() {
            return;
        }
        let _ = self.client.refresh_grid(pane);
        // Decided when the grid lands (`apply_grid_snap`); this is for one
        // that never comes, on what the pane last showed here.
        let pane = pane.to_string();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                if let Some(loud) = app.screen_check.remove(&pane) {
                    app.turn_done(&pane, loud, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn turn_done(&mut self, pane: &str, loud: Option<u64>, cx: &mut Context<Self>) {
        // Closed or slept while its screen was on the way.
        if !self.panes.iter().any(|p| p.slug == pane && !p.asleep) {
            return;
        }
        // Busy again while its screen was on the way: that turn's end says.
        if self.busy_panes.contains(pane) {
            return;
        }
        let compacted = self
            .panes
            .iter()
            .find(|p| p.slug == pane)
            .and_then(|p| p.remote_terminal())
            .is_some_and(|t| super::stage::idle_compacted(&t.read(cx).snapshot));
        if compacted {
            return;
        }
        self.mark_done_unseen(pane, loud.is_some());
        cx.notify();
        if let Some(took) = loud {
            let body = if took >= 60 {
                format!("finished after {}m {}s", took / 60, took % 60)
            } else {
                format!("finished after {took}s")
            };
            self.on_pane_alert(pane, "done", "", &body, 0, None);
        }
    }

    /// A Claude pane finished out of sight: ✓ on its row, and the circle's
    /// "done" mark when the human is in another circle (so the alert says
    /// where to look, not just that something happened).
    /// `loud`: a turn long enough for the lead's ding.
    pub(super) fn mark_done_unseen(&mut self, pane: &str, loud: bool) {
        let watching = self.window_active && self.active_slug.as_deref() == Some(pane);
        if watching {
            return;
        }
        self.done_unseen.insert(pane.to_string());
        let Some(ws) = self
            .panes
            .iter()
            .find(|p| p.slug == pane)
            .map(|p| p.workspace.clone())
        else {
            return;
        };
        // Only a circle's lead dings (its crew finishing is the lead's
        // business), and on every finish of 5s or more, like the done
        // banner: one still unseen from before doesn't make the next one
        // silent.
        if self.stage.leads.get(&ws).map(String::as_str) == Some(pane) {
            if loud && !self.snoozed(pane) {
                desktop_notify::done_sound();
            }
        } else {
            // Crew: the lead's card says so, without a word to the lead.
            self.crew_done
                .insert(pane.to_string(), std::time::Instant::now());
        }
        if self.selected_workspace.as_deref() == Some(ws.as_str()) {
            return;
        }
        self.workspace_unread.insert(ws, WorkspaceAttention::Done);
    }

    /// Snoozed on Stage right now.
    pub(super) fn snoozed(&self, slug: &str) -> bool {
        self.snoozes.on(slug, now_ms())
    }

    /// Stage's "Snooze 1h" on a pane.
    pub(super) fn snooze(&mut self, slug: &str) {
        self.snoozes.add(slug, now_ms());
        self.save_snoozes();
    }

    pub(super) fn unsnooze_all(&mut self) {
        self.snoozes.0.clear();
        self.save_snoozes();
    }

    /// Kept in this machine's view.json, from the window that keeps its
    /// layout there.
    fn save_snoozes(&mut self) {
        self.snoozes.prune(now_ms());
        if self.view.persist {
            super::views::save_snoozes(&self.snoozes.0);
        }
    }

    /// What a notification calls a pane: "circle · name", the names the human
    /// gave them. Unknown slugs (an asker that isn't a pane) pass through.
    pub(super) fn pane_label(&self, pane: &str) -> String {
        self.panes
            .iter()
            .find(|p| p.slug == pane)
            .map(|p| format!("{} · {}", self.workspace_label(&p.workspace), p.name))
            .unwrap_or_else(|| pane.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snooze_lasts_an_hour_and_opening_ends_it() {
        let mut s = Snoozes::default();
        s.add("w", 1_000);
        assert!(s.on("w", 1_000 + SNOOZE_MS - 1));
        assert!(!s.on("w", 1_000 + SNOOZE_MS));
        assert!(!s.on("other", 1_000));
        // Opening ends it early, once.
        assert!(s.clear("w", 2_000));
        assert!(!s.on("w", 2_000));
        assert!(!s.clear("w", 2_000));
        // Expired ones are dropped before saving.
        s.add("a", 0);
        s.add("b", 5_000);
        s.prune(SNOOZE_MS + 1);
        assert_eq!(s.0.keys().collect::<Vec<_>>(), vec!["b"]);
    }
}
