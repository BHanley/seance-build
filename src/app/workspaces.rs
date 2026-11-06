//! Workspace state operations: circle order (the order you drag Stage's
//! pills into), attention/unread bookkeeping, pane drag-reorder,
//! workspace lifecycle (create / select / cycle / move / fork / kill), and the
//! back/forward visit history the mouse's side buttons walk.
//! Pure state — no rendering lives here (the sidebar/overview views call
//! these to compute their layout).

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{Context, Window};

use super::util::now_ms;
use crate::runtime::protocol::GuiRequest;
use seance_core::util::{rail_prefs_is_foreign, settle_absent};

/// How long the sidebar banish × stays armed after a first click. Matches the
/// web client's `KILL_CONFIRM_MS` so both clients feel the same.
pub(super) const BANISH_ARM: Duration = Duration::from_millis(2000);
/// Longer than a row's ×: the confirm pill appears after a menu closes, so
/// the hand has further to travel.
pub(super) const SPACE_BANISH_ARM: Duration = Duration::from_millis(5000);

/// Arm key for a space's banish-all; shares `banish_armed` with row ×s so
/// only one destructive control is ever hot.
pub(super) fn space_banish_key(id: &str) -> String {
    format!("space:{id}")
}

/// Is `ws` the armed circle, with the arm still live at `now`? Pure so the
/// two-click window is testable without leaning on a real clock.
pub(super) fn banish_arm_live(armed: Option<&(String, Instant)>, ws: &str, now: Instant) -> bool {
    armed.is_some_and(|(w, at)| w == ws && now.duration_since(*at) < BANISH_ARM)
}

/// Is this pane command a Claude Code launch (`claude`, `claude-mac`, a
/// path to either, optionally behind `env VAR=…`)?
pub(super) fn runs_claude(command: &str) -> bool {
    command
        .split_whitespace()
        .find(|t| *t != "env" && !t.contains('='))
        .and_then(|t| t.rsplit('/').next())
        .is_some_and(|b| b == "claude" || b == "claude-mac")
}

/// Coarse one-unit relative time for sidebar labels.
pub(super) fn rel_label(delta_ms: u64) -> String {
    let s = delta_ms / 1000;
    match s {
        0..=4 => "now".into(),
        5..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}
use super::{RenameTarget, SeanceApp};

/// Circle `moved` dropped onto `target` takes `target`'s place: after it when
/// dragged right, before it when dragged left. Returns the new order and the
/// circle the daemon should put `moved` before ("" for the end), or None when
/// nothing moves.
pub(super) fn drop_circle(
    order: &[String],
    moved: &str,
    target: &str,
) -> Option<(Vec<String>, String)> {
    let from = order.iter().position(|w| w == moved)?;
    let to = order.iter().position(|w| w == target)?;
    if from == to {
        return None;
    }
    let before = if from < to {
        order.get(to + 1).cloned().unwrap_or_default()
    } else {
        target.to_string()
    };
    let mut out: Vec<String> = order.iter().filter(|w| *w != moved).cloned().collect();
    let at = out.iter().position(|w| *w == before).unwrap_or(out.len());
    out.insert(at, moved.to_string());
    Some((out, before))
}

/// How many circles back the mouse can walk. A long day of cycling shouldn't
/// grow a list forever, and nobody navigates back past a few dozen hops.
const NAV_HISTORY_MAX: usize = 64;

/// Browser-style visit history over circles — where you've *been*, in order,
/// with a cursor at where you are now.
///
/// Deliberately not the same thing as the jump palette's recency ranking:
/// recency is a set sorted by a clock, this is a path with a position in it,
/// so back-then-forward returns you to exactly the circle you left. Per
/// window, never persisted — a fresh window starts with no history, like a
/// fresh browser tab.
///
/// The invariant that makes this work with a passive observer:
/// `entries[cursor]` is always the circle currently on screen. So a selection
/// that *isn't* `entries[cursor]` is by definition a fresh navigation, and
/// walking back/forward moves the cursor first — which is what keeps the
/// observer from mistaking our own step for a new visit and eating the
/// forward half of the history.
#[derive(Default)]
pub(super) struct NavHistory {
    entries: Vec<String>,
    cursor: usize,
}

impl NavHistory {
    /// Fold the currently-selected circle in. A no-op when it's already where
    /// the cursor sits; otherwise it's a fresh navigation, which drops
    /// whatever was ahead (same as clicking a link mid-history in a browser).
    pub(super) fn visit(&mut self, ws: &str) {
        if self.entries.get(self.cursor).map(String::as_str) == Some(ws) {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(ws.to_string());
        if self.entries.len() > NAV_HISTORY_MAX {
            self.entries.drain(..self.entries.len() - NAV_HISTORY_MAX);
        }
        self.cursor = self.entries.len() - 1;
    }

    /// Step the cursor back to the nearest circle that still exists, and
    /// report it. Banished circles are stepped over rather than pruned —
    /// keeping the indices stable is what lets forward retrace the same path.
    pub(super) fn back(&mut self, exists: impl Fn(&str) -> bool) -> Option<String> {
        let mut i = self.cursor;
        while i > 0 {
            i -= 1;
            if exists(&self.entries[i]) {
                self.cursor = i;
                return Some(self.entries[i].clone());
            }
        }
        None
    }

    /// The inverse, toward the newest end.
    pub(super) fn forward(&mut self, exists: impl Fn(&str) -> bool) -> Option<String> {
        let mut i = self.cursor;
        while i + 1 < self.entries.len() {
            i += 1;
            if exists(&self.entries[i]) {
                self.cursor = i;
                return Some(self.entries[i].clone());
            }
        }
        None
    }
}

/// What the rail's circle list is narrowed to. A view, never a position:
/// filtering moves nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RailFilter {
    All,
    Starred,
    /// A host circle mode (AFK, slack thread) by id.
    Mode(String),
}

/// Badge on an *inactive* workspace header in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceAttention {
    /// Observed live-busy (TUI title spinner / agent actively driving).
    Working,
    /// A pane is live blocked or needs-human (never sticky): the blinking
    /// needs-you dot.
    NeedsHuman,
    /// Finished work while the human was elsewhere — sticky until select.
    Done,
    /// A pane you marked unread to come back to; until you open it.
    Later,
}

impl WorkspaceAttention {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::NeedsHuman => "needs",
            Self::Done => "done",
            Self::Later => "unread",
        }
    }
    pub(super) fn color(self) -> gpui::Hsla {
        match self {
            Self::Working => crate::theme::SeancePalette::flame(),
            Self::NeedsHuman => crate::theme::SeancePalette::violet(),
            Self::Done => crate::theme::SeancePalette::success(),
            Self::Later => crate::theme::SeancePalette::text(),
        }
    }
    pub(super) fn priority(self) -> u8 {
        match self {
            Self::NeedsHuman => 3,
            Self::Working => 2,
            Self::Done => 1,
            Self::Later => 0,
        }
    }
}

impl SeanceApp {
    /// Unsorted set of workspace names this window knows about.
    pub(super) fn known_workspace_names(&self) -> std::collections::HashSet<String> {
        self.panes
            .iter()
            .map(|s| s.workspace.clone())
            .chain(self.extra_workspaces.iter().cloned())
            .chain(self.selected_workspace.iter().cloned())
            .collect()
    }

    /// Fold a fresh `State` into this window's persisted arrangement, and make
    /// sure the daemon is streaming every circle it knows about.
    ///
    /// Every non-blank window subscribes to everything (0.26, when park went
    /// away), so the only reason this still touches subscriptions is catch-up:
    /// a circle created or renamed since the last `State` isn't in the
    /// connection's set until we ask for it.
    pub(super) fn reconcile_subscriptions(&mut self, known: &std::collections::BTreeSet<String>) {
        let subs = self.subscriptions.clone();
        let mut changed = false;
        if !self.subs_seeded {
            // Fresh install: everything that already exists counts as
            // looked-at, so the rail doesn't come up all badged.
            self.subs_pref.seed_seen(known);
            self.subs_seeded = true;
            changed = true;
        }
        // Selecting is looking: never badge the circle you're in as unseen.
        if let Some(sel) = self.selected_workspace.clone() {
            changed |= self.subs_pref.mark_seen(&sel);
        }
        let referenced: Vec<String> = self
            .subs_pref
            .pinned
            .iter()
            .chain(self.subs_pref.seen.iter())
            .chain(self.subs_pref.spaces.of.keys())
            .cloned()
            .collect();
        let protected = settle_absent(
            &mut self.absent_since,
            referenced.iter().map(String::as_str),
            known,
            now_ms(),
        );
        changed |= self.subs_pref.prune(&protected);
        // Cluster folds died with the clusters (spaces replaced them).
        changed |= self
            .subs_pref
            .prune_collapsed(&std::collections::BTreeSet::new());
        // `protected`, not `known`: a circle a lagging State omits keeps its
        // space instead of being forgotten and re-routed.
        let circles: Vec<(String, String)> = protected
            .iter()
            .map(|ws| (ws.clone(), self.workspace_label(ws)))
            .collect();
        let focused = self.current_space();
        if self.subs_pref.spaces.reconcile(&circles, Some(&focused)) {
            if self.rail_loaded {
                self.save_arrangement();
            } else {
                self.save_arrangement_local();
            }
        }
        // Then freeze the view on the selected circle's space: a fresh circle
        // is selected before it has one, and following the selection then
        // would send it to the default. Only once the selection is assigned —
        // freezing before the first migration pinned the rail to `general`.
        if self.rail_space.is_none() {
            if let Some(sel) = self.selected_workspace.as_deref() {
                if self.subs_pref.spaces.of.contains_key(sel) {
                    self.rail_space = Some(self.subs_pref.spaces.space_of(sel).to_string());
                }
            }
        }
        if changed {
            self.save_arrangement_local();
        }
        // Anything the daemon isn't streaming yet (reconnect, rename, a circle
        // ctl just spawned) gets subscribed so its grids flow.
        if !self.empty_window {
            let missing: Vec<String> = known
                .iter()
                .filter(|w| !subs.iter().any(|s| s == *w))
                .cloned()
                .collect();
            for ws in missing {
                let _ = self.client.subscribe(&ws);
            }
        }
    }

    /// Persist a DELIBERATE arrangement change — pin, unpin, a fold you
    /// clicked, the notes face — locally and to the daemon, which shares it
    /// with every other window.
    ///
    /// **`empty_window` must never gate anything on this path.** Blank-ness is
    /// about what a window ATTACHES to, not about whether your choices count.
    /// The gate lived in three places and removing two of them fixed nothing:
    /// the local cache was written, `push_rail_to_daemon` still bailed, and
    /// boot loads the DAEMON's copy over the local one — so every pin placed in
    /// a blank window rendered, persisted nowhere that boot reads, and was gone
    /// on restart. What protects the shared copy is the deliberate/incidental
    /// split below, not the window's blank-ness.
    pub(super) fn save_arrangement(&self) {
        crate::subscriptions_pref::save(&self.subs_pref);
        self.push_rail_to_daemon();
    }

    /// Persist incidental bookkeeping — `seen`, a prune, a fold opened just to
    /// reveal a row — to the LOCAL cache only.
    ///
    /// These fire on selection and on every `State`, so pushing them raced the
    /// deliberate changes: a window whose copy predated your pin by
    /// milliseconds would push its own arrangement over the top and the daemon
    /// would broadcast the pin away. Nothing here is worth another window's
    /// attention; the next real change carries it along.
    pub(super) fn save_arrangement_local(&self) {
        crate::subscriptions_pref::save(&self.subs_pref);
    }

    /// Hand the arrangement to the daemon, which persists it and pushes it to
    /// every other window.
    ///
    /// Off the UI thread on purpose: this is a blocking bridge round trip and
    /// every caller is a click — pinning a circle must not wait on a socket,
    /// least of all over ssh from the mac.
    ///
    /// Through one long-lived writer, NOT a thread per call. A thread per call
    /// has no ordering, so pin-then-unpin could land unpin-then-pin; the daemon
    /// would persist the loser and broadcast it back, and the pin visibly
    /// undid itself. The queue also coalesces: only the newest arrangement is
    /// worth writing.
    pub(super) fn push_rail_to_daemon(&self) {
        let Some(json) = crate::subscriptions_pref::encode(&self.subs_pref) else {
            return;
        };
        if let Ok(mut last) = self.rail_push.last_sent.lock() {
            *last = Some(json.clone());
        }
        self.rail_push.pending.fetch_add(1, Ordering::SeqCst);
        if self.rail_push.tx.send(json).is_err() {
            self.rail_push.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Should an inbound `RailPrefs` be adopted, or is it our own change coming
    /// back around?
    ///
    /// The daemon broadcasts to every window INCLUDING the sender, so without
    /// this a window overwrites its own fresh state with an older copy of it.
    pub(super) fn rail_prefs_is_foreign(&self, json: &str) -> bool {
        let last = self.rail_push.last_sent.lock().ok();
        rail_prefs_is_foreign(
            self.rail_push.pending.load(Ordering::SeqCst),
            last.as_ref().and_then(|l| l.as_deref()),
            json,
        )
    }

    /// Pin a circle to the top section (context menu "pin to top", and every
    /// quicklaunch click, which pins before the spawn round trip returns —
    /// see [`seance_core::util::ABSENT_GRACE_MS`]).
    pub(super) fn pin_workspace(&mut self, ws: &str) {
        if self.subs_pref.pin(ws) {
            self.save_arrangement();
        }
    }

    /// Drop a circle out of the pinned section. It falls back below the
    /// divider into its lifecycle band.
    pub(super) fn unpin_workspace(&mut self, ws: &str) {
        if self.subs_pref.unpin(ws) {
            self.save_arrangement();
        }
    }

    /// Flip the selected circle's pin (ctrl+shift+p). Same two calls the row
    /// menu makes — the chord is just a faster way to reach them, and the rail
    /// moving the row between bands is the feedback.
    pub(super) fn toggle_pin_workspace(&mut self, ws: &str) {
        if self.subs_pref.is_pinned(ws) {
            self.unpin_workspace(ws);
        } else {
            self.pin_workspace(ws);
        }
    }

    /// The host modes on for `workspace`, as their definitions (badge labels).
    pub(super) fn circle_modes_of(
        &self,
        workspace: &str,
    ) -> Vec<seance_core::protocol::CircleModeDef> {
        let on = self.circle_modes.get(workspace);
        self.circle_mode_defs
            .iter()
            .filter(|d| on.is_some_and(|m| m.contains(&d.id)))
            .cloned()
            .collect()
    }

    /// The space the rail is showing: the one picked, else the selected
    /// circle's, else the default.
    pub(super) fn current_space(&self) -> String {
        let spaces = &self.subs_pref.spaces;
        if let Some(id) = self.rail_space.as_deref() {
            if spaces.get(id).is_some() {
                return id.to_string();
            }
        }
        match self.selected_workspace.as_deref() {
            Some(ws) => spaces.space_of(ws).to_string(),
            None => spaces.default_id().to_string(),
        }
    }

    pub(super) fn rail_filter_matches(&self, filter: &RailFilter, ws: &str) -> bool {
        match filter {
            RailFilter::All => true,
            RailFilter::Starred => self.subs_pref.is_pinned(ws),
            RailFilter::Mode(id) => self
                .circle_modes
                .get(ws)
                .is_some_and(|m| m.iter().any(|x| x == id)),
        }
    }

    /// Every circle the rail is listing, top-to-bottom. Unfiltered: the
    /// current space. Filtered: matches from every space, in space order.
    /// This is the ctrl+page ring and the ctrl+shift+N count.
    pub(super) fn visible_workspaces(&self) -> Vec<String> {
        let ordered = self.workspaces();
        let spaces = &self.subs_pref.spaces;
        match &self.rail_filter {
            RailFilter::All => spaces.circles_in(&self.current_space(), &ordered),
            f => spaces
                .list
                .iter()
                .flat_map(|s| spaces.circles_in(&s.id, &ordered))
                .filter(|ws| self.rail_filter_matches(f, ws))
                .collect(),
        }
    }

    /// Rows the rail scroller emits above the circle list: one per space and
    /// the divider.
    pub(super) fn rail_rows_above_circles(&self) -> usize {
        self.subs_pref.spaces.list.len() + 1
    }

    /// Bring a circle's row into view: switch the rail to its space (unless a
    /// filter is already showing it), then scroll.
    ///
    /// Called on every select, so ctrl+page cycling, a jump, and a host menu
    /// creating a circle all land the same way.
    pub(super) fn reveal_workspace_row(&mut self, workspace: &str) {
        let assigned = self.subs_pref.spaces.of.contains_key(workspace);
        if assigned && !self.visible_workspaces().iter().any(|w| w == workspace) {
            self.rail_filter = RailFilter::All;
            self.rail_space = Some(self.subs_pref.spaces.space_of(workspace).to_string());
        }
        if let Some(i) = self
            .visible_workspaces()
            .iter()
            .position(|w| w == workspace)
        {
            self.sidebar_scroll
                .scroll_to_item(self.rail_rows_above_circles() + i);
        }
    }

    /// Strongest attention among a space's circles, for its rail row.
    pub(super) fn space_attention(&self, id: &str) -> Option<WorkspaceAttention> {
        let ordered = self.workspaces();
        self.subs_pref
            .spaces
            .circles_in(id, &ordered)
            .iter()
            .filter_map(|ws| self.row_attention(ws))
            .max_by_key(|a| a.priority())
    }

    /// A space's mark as its rail row draws it: the blinking needs-you dot,
    /// the working spinner, else the attention's dot. None when quiet.
    pub(super) fn space_mark(&self, id: &str) -> Option<(&'static str, gpui::Hsla)> {
        Some(match self.space_attention(id)? {
            WorkspaceAttention::Working => (
                super::util::working_spinner_glyph(),
                crate::theme::SeancePalette::flame(),
            ),
            WorkspaceAttention::NeedsHuman => ("\u{25cf}", super::util::needs_dot_color()),
            a => ("\u{25cf}", a.color()),
        })
    }

    pub(super) fn move_circle_to_space(&mut self, ws: &str, space: &str) {
        if self.subs_pref.spaces.move_circle(ws, space) {
            self.client.log_event(
                "human",
                Some(ws),
                None,
                "workspace_moved_space",
                format!("moved '{ws}' to space '{space}'"),
            );
            self.save_arrangement();
        }
    }

    pub(super) fn create_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.subs_pref.spaces.create("new space");
        self.save_arrangement();
        self.rail_space = Some(id.clone());
        self.rail_filter = RailFilter::All;
        self.start_rename(super::RenameTarget::Space(id), "new space", window, cx);
    }

    pub(super) fn delete_space(&mut self, id: &str) {
        if self.subs_pref.spaces.delete(id) {
            if self.rail_space.as_deref() == Some(id) {
                self.rail_space = None;
            }
            self.save_arrangement();
        }
    }

    pub(super) fn rename_space(&mut self, id: &str, name: &str) {
        if self.subs_pref.spaces.rename(id, name) {
            self.save_arrangement();
        }
    }

    /// Show a space and land on its first circle.
    pub(super) fn show_space(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.rail_space = Some(id.to_string());
        self.rail_filter = RailFilter::All;
        if let Some(first) = self.visible_workspaces().into_iter().next() {
            if self.selected_workspace.as_deref() != Some(first.as_str()) {
                self.select_workspace(&first, window, cx);
            }
        }
    }

    /// The space Stage and Wall show: Stage's strip follows the selected
    /// circle so its pill is always there; the rest follow the rail.
    pub(super) fn shown_space(&self) -> String {
        match (self.view.mode, self.selected_workspace.as_deref()) {
            (super::views::ViewMode::Stage, Some(ws)) => {
                self.subs_pref.spaces.space_of(ws).to_string()
            }
            _ => self.current_space(),
        }
    }

    /// A pick from Stage or Wall's space tabs, or ctrl+alt+page: show the
    /// space and, on Stage, its first circle's lead. Leaves Wall's All.
    pub(super) fn pick_space(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A tab can name a space deleted since it was drawn; the space on
        // show stays as it is, its circle and pane included.
        if self.subs_pref.spaces.get(id).is_none() {
            cx.notify();
            return;
        }
        if self.wall_all() {
            self.set_wall_all(false);
            // The selected circle stays when it lives in this space.
            let spaces = &self.subs_pref.spaces;
            if self
                .selected_workspace
                .as_deref()
                .is_some_and(|ws| spaces.space_of(ws) == id)
            {
                self.rail_space = Some(id.to_string());
                cx.notify();
                return;
            }
        } else if self.shown_space() == id {
            cx.notify();
            return;
        }
        self.show_space(id, window, cx);
        if self.view.mode == super::views::ViewMode::Stage {
            self.stage.want_lead = true;
        }
        cx.notify();
    }

    /// ctrl+alt+page: the previous or next space with circles in it. Empty
    /// spaces are skipped; Stage would have nothing to put up.
    pub(super) fn cycle_space(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let ordered = self.workspaces();
        let spaces = &self.subs_pref.spaces;
        let counts: Vec<usize> = spaces
            .list
            .iter()
            .map(|s| spaces.circles_in(&s.id, &ordered).len())
            .collect();
        let shown = self.shown_space();
        let cur = spaces.list.iter().position(|s| s.id == shown);
        if let Some(i) = super::stage_parse::step_space(&counts, cur, delta) {
            let id = spaces.list[i].id.clone();
            self.pick_space(&id, window, cx);
        }
    }

    pub(super) fn space_banish_armed(&self, id: &str) -> bool {
        self.banish_armed
            .as_ref()
            .is_some_and(|(k, at)| *k == space_banish_key(id) && at.elapsed() < SPACE_BANISH_ARM)
    }

    /// First call arms (the row shows a confirm pill), a second inside
    /// `SPACE_BANISH_ARM` kills every circle in the space.
    pub(super) fn banish_space_click(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.space_banish_armed(id) {
            self.banish_armed = None;
            let circles = self.subs_pref.spaces.circles_in(id, &self.workspaces());
            let lost_selection = self
                .selected_workspace
                .as_ref()
                .is_some_and(|s| circles.contains(s));
            self.client.log_event(
                "human",
                None,
                None,
                "space_banished",
                format!("banished {} circle(s) in space '{id}'", circles.len()),
            );
            for ws in &circles {
                let _ = self.client.kill_workspace(ws);
            }
            if lost_selection {
                let next = self.workspaces().into_iter().find(|w| !circles.contains(w));
                if let Some(next) = next {
                    self.select_workspace(&next, window, cx);
                }
                // Stay on the emptied space; the rail shows it empty.
                self.rail_space = Some(id.to_string());
            }
            return;
        }
        self.banish_armed = Some((space_banish_key(id), Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SPACE_BANISH_ARM).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |app: &mut SeanceApp, cx| {
                    if app
                        .banish_armed
                        .as_ref()
                        .is_some_and(|(_, at)| at.elapsed() >= SPACE_BANISH_ARM)
                    {
                        app.banish_armed = None;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    pub(super) fn space_set_default(&mut self, id: &str) {
        if self.subs_pref.spaces.set_default(id) {
            self.save_arrangement();
        }
    }

    /// Nudge a space one slot up (`-1`) or down (`+1`) in the rail.
    pub(super) fn space_shift(&mut self, id: &str, delta: i32) {
        let list = &self.subs_pref.spaces.list;
        let Some(i) = list.iter().position(|s| s.id == id) else {
            return;
        };
        let before = if delta < 0 {
            if i == 0 {
                return;
            }
            Some(list[i - 1].id.clone())
        } else {
            match list.get(i + 2) {
                Some(s) => Some(s.id.clone()),
                None if i + 1 < list.len() => None,
                None => return,
            }
        };
        if self.subs_pref.spaces.reorder(id, before.as_deref()) {
            self.save_arrangement();
        }
    }

    /// Badge for a rail row. `NeedsHuman` comes only from a live
    /// needs-human, blocked or risky pane, never from a sticky unread.
    pub(super) fn row_attention(&self, ws: &str) -> Option<WorkspaceAttention> {
        self.workspace_attention_cx(ws)
    }

    /// All workspaces in display order: the order you dragged them into on
    /// Stage (the daemon's `workspace_order`, shared by every window), then
    /// any it doesn't list yet by name. Rows never move on their own.
    pub(super) fn workspaces(&self) -> Vec<String> {
        let mut out: Vec<String> = self.known_workspace_names().into_iter().collect();
        out.sort_by_key(|ws| {
            let at = self.workspace_order.iter().position(|w| w == ws);
            (at.unwrap_or(usize::MAX), ws.to_lowercase())
        });
        out
    }

    /// Drop circle `moved` onto `target` (see [`drop_circle`]). The new
    /// order shows at once; the daemon's State push confirms it.
    pub(super) fn reorder_circle(&mut self, moved: &str, target: &str) {
        let Some((order, before)) = drop_circle(&self.workspaces(), moved, target) else {
            return;
        };
        self.workspace_order = order;
        let _ = self.client.send(GuiRequest::ReorderWorkspace {
            moved: moved.to_string(),
            before,
        });
    }

    /// Is an agent working in this circle right now?
    ///
    /// ONLY the title spinner, which in practice means claude. Output recency
    /// looks like a universal signal and is not: codex repaints its TUI on a
    /// timer, so its output clock never goes stale and every codex circle read
    /// as permanently working. codex puts no working state in its title
    /// either, so for a non-spinner agent the honest answer is "unknown".
    pub(super) fn workspace_has_working_agent(&self, workspace: &str) -> bool {
        self.panes
            .iter()
            .any(|p| p.workspace == workspace && self.pane_is_live_working(&p.slug))
    }

    /// Live-busy, as the DAEMON sees it: braille OSC title spinner, or
    /// agent-owned status.
    ///
    /// The spinner half deliberately does *not* read the local terminal title.
    /// Grid frames only arrive for the workspace this window has selected, so
    /// every other circle's title is frozen at whatever it last received —
    /// which is exactly the spinner it was wearing when you looked away. The
    /// daemon broadcasts busy flips for every pane instead.
    fn pane_is_live_working(&self, slug: &str) -> bool {
        if self.busy_panes.contains(slug) {
            return true;
        }
        // Claude panes (claude, claude-mac) always wear the title spinner
        // while busy, so the daemon's busy flag is the whole answer. Their
        // "working" status goes stale: Mac panes have no hook to clear it, and
        // an agent's inject leaves it set, which spun a circle forever.
        if self
            .panes
            .iter()
            .any(|p| p.slug == slug && runs_claude(&p.command))
        {
            return false;
        }
        let owner = self.owners.get(slug);
        let st = self.statuses.get(slug).map(|s| s.state.as_str());
        match (owner, st) {
            // Human-owned sticky "working" is often stale inject chrome — ignore.
            (Some(o), Some("working") | Some("planning")) if o.owner == "human" => false,
            (_, Some("working") | Some("planning")) => true,
            (Some(o), _) if o.owner.starts_with("agent:") && !o.exited => {
                // Agent holds keys without status-set — still "live" if title busy already handled.
                false
            }
            _ => false,
        }
    }

    /// Wake a circle AND land the keyboard in it.
    ///
    /// Every awaken affordance is a click, and a click leaves focus on the
    /// thing you clicked (the bar button, the context-menu item) — so without
    /// this you'd have to click the pane before typing. The pane view already
    /// exists (sleeping never unmounted it); `pending_focus` survives the
    /// round-trip and is applied on the first render after the daemon relaunches.
    pub(super) fn wake_workspace_focused(
        &mut self,
        ws: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.client.wake_workspace(ws);
        if let Some(slug) = self.preferred_pane_in_workspace(ws) {
            self.set_active(&slug, window, cx);
            self.pending_focus = Some(slug);
        }
        cx.notify();
    }

    /// What to show for a circle. Its slug is the identity; this is the
    /// mutable label, and it falls back to the slug — which is exactly what a
    /// circle reads as until someone renames it.
    pub(super) fn workspace_label(&self, ws: &str) -> String {
        self.workspace_names
            .get(ws)
            .cloned()
            .unwrap_or_else(|| ws.to_string())
    }

    /// The circle reads as asleep when all its terminals are. The daemon
    /// sleeps idle Claude panes one at a time, so a circle with one asleep
    /// beside a live shell or Mac pane is still awake; that pane shows its
    /// own ☾ and wakes on a keystroke or Stage's awaken.
    pub(super) fn workspace_asleep(&self, ws: &str) -> bool {
        let mut terms = self
            .panes
            .iter()
            .filter(|p| p.workspace == ws && !matches!(p.body, crate::pane::PaneBody::File { .. }))
            .peekable();
        terms.peek().is_some() && terms.all(|p| p.asleep)
    }

    /// Every pane in the circle can be put back exactly (daemon's verdict, on
    /// the wire as `PaneInfo::restorable`). Gates the "sleep circle" verb.
    pub(super) fn workspace_sleepable(&self, ws: &str) -> bool {
        let mut any = false;
        for p in self.panes.iter().filter(|p| p.workspace == ws) {
            any = true;
            if !p.restorable {
                return false;
            }
        }
        any
    }

    /// Bump this circle's recency for the palette's jump order. Sources: human
    /// typing into a terminal here, newly created circles, and the moment a
    /// workspace *finishes* working.
    pub(super) fn touch_workspace(&mut self, ws: &str) {
        if ws.is_empty() {
            return;
        }
        self.workspace_touch.insert(ws.to_string(), now_ms());
    }

    /// Recompute live-working per workspace. When a circle stops having any
    /// working agent, bump its touch (freshly finished work is what the
    /// palette offers next).
    pub(super) fn sync_workspace_working_touches(&mut self) {
        let names: Vec<String> = self.known_workspace_names().into_iter().collect();
        for ws in names {
            // No touch bump on the falling edge any more. Working is keyed off
            // output recency now, so a circle that just went quiet already
            // holds the freshest clock in the idle band and lands at the top
            // on its own — bumping it was a second, redundant reorder, and it
            // fired for circles whose edge nobody observed.
            if self.workspace_has_working_agent(&ws) {
                self.workspace_was_working.insert(ws);
            } else {
                self.workspace_was_working.remove(&ws);
            }
        }
    }

    /// Track a newly known workspace name at the end of the circle order.
    pub(super) fn ensure_workspace_at_bottom(&mut self, ws: &str) {
        if self.workspace_order.iter().any(|w| w == ws) {
            return;
        }
        self.workspace_order.push(ws.to_string());
        self.touch_workspace(ws);
    }

    pub(super) fn note_workspace_status_event(&mut self, slug: &str, state: &str) {
        let Some(ws) = self
            .panes
            .iter()
            .find(|p| p.slug == slug)
            .map(|p| p.workspace.clone())
        else {
            return;
        };
        // Status changes do *not* bump touch — only human typing / explicit
        // touch menu. Working agents re-sort via live-busy detection.
        // Sticky unread only when the human is *not* looking at this circle.
        if self.selected_workspace.as_deref() == Some(ws.as_str()) {
            self.workspace_unread.remove(&ws);
            return;
        }
        // Only done leaves a mark: working is read live, and a sticky one
        // would outrank Done.
        let att = match state {
            "done" => Some(WorkspaceAttention::Done),
            _ => None,
        };
        if let Some(a) = att {
            let cur = self.workspace_unread.get(&ws).copied();
            if cur.map(|c| a.priority() > c.priority()).unwrap_or(true) {
                self.workspace_unread.insert(ws, a);
            }
        }
    }

    /// One pane's glyph for its sidebar row: needs-human, else working.
    pub(super) fn pane_attention(&self, slug: &str) -> Option<WorkspaceAttention> {
        let needs = matches!(
            self.statuses.get(slug).map(|s| s.state.as_str()),
            Some("needs-human") | Some("blocked") | Some("risky")
        );
        if needs {
            Some(WorkspaceAttention::NeedsHuman)
        } else if self.pane_is_live_working(slug) {
            Some(WorkspaceAttention::Working)
        } else {
            None
        }
    }

    /// Live attention with title spinners (needs `&App`), for badges.
    pub(super) fn workspace_attention_cx(&self, workspace: &str) -> Option<WorkspaceAttention> {
        // A pane asking for a human right now (live status, never sticky) is
        // the one purple the rail keeps: it drives the blinking needs-you dot.
        if self.panes.iter().any(|p| {
            p.workspace == workspace
                && matches!(
                    self.statuses.get(&p.slug).map(|s| s.state.as_str()),
                    Some("needs-human") | Some("blocked") | Some("risky")
                )
        }) {
            return Some(WorkspaceAttention::NeedsHuman);
        }
        if self.workspace_has_working_agent(workspace) {
            return Some(WorkspaceAttention::Working);
        }
        self.workspace_unread.get(workspace).copied().or_else(|| {
            self.panes
                .iter()
                .any(|p| p.workspace == workspace && self.subs_pref.later.contains(&p.slug))
                .then_some(WorkspaceAttention::Later)
        })
    }

    /// Sidebar right-edge label: relative time since the last pane output in
    /// this circle ("now", "42s", "3m", "2h", "4d"); None while a working
    /// agent's spinner owns the slot, or when nothing was ever observed.
    pub(super) fn workspace_activity_label(&self, ws: &str) -> Option<String> {
        if self.workspace_has_working_agent(ws) {
            return None;
        }
        let at = *self.workspace_activity.get(ws)?;
        Some(rel_label(now_ms().saturating_sub(at)))
    }

    /// Move `slug` into `workspace`, positioned before pane `before_slug`
    /// (or appended when `before_slug` is None). Optimistic local reorder;
    /// daemon reorders + persists and pushes State back.
    pub(super) fn reorder_pane(
        &mut self,
        slug: &str,
        workspace: &str,
        before_slug: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if Some(slug) == before_slug {
            return;
        }
        let Some(from_idx) = self.panes.iter().position(|p| p.slug == slug) else {
            return;
        };
        let mut pane = self.panes.remove(from_idx);
        pane.workspace = workspace.to_string();
        let insert_at = before_slug
            .and_then(|b| self.panes.iter().position(|p| p.slug == b))
            .unwrap_or(self.panes.len());
        self.client.log_event(
            "human",
            Some(workspace),
            Some(slug),
            "pane_moved",
            format!("moved '{}' into {} (reorder)", pane.name, workspace),
        );
        self.panes.insert(insert_at, pane);
        self.selected_workspace = Some(workspace.to_string());
        let _ = self.client.move_pane(slug, workspace, before_slug);
        cx.notify();
    }

    /// Title-strip drag within a circle: `slug` takes `target`'s spot and
    /// the panes between shift over one.
    pub(super) fn move_pane_to_spot(&mut self, slug: &str, target: &str, cx: &mut Context<Self>) {
        let pos = |s: &str| self.panes.iter().position(|p| p.slug == s);
        let (Some(from), Some(to)) = (pos(slug), pos(target)) else {
            return;
        };
        let ws = self.panes[to].workspace.clone();
        if from == to || self.panes[from].workspace != ws {
            return;
        }
        // Moving later: land after the target, i.e. before the next pane in
        // this circle (or at the end).
        let before = if from < to {
            self.panes[to + 1..]
                .iter()
                .find(|p| p.workspace == ws)
                .map(|p| p.slug.clone())
        } else {
            Some(target.to_string())
        };
        self.reorder_pane(slug, &ws, before.as_deref(), cx);
    }

    /// A new circle in the space on show, its name open for typing. Returns
    /// its slug.
    pub(super) fn create_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> String {
        let existing = self.known_workspace_names();
        let mut n = existing.len() + 1;
        let name = loop {
            let candidate = format!("circle-{n}");
            if !existing.contains(&candidate) {
                break candidate;
            }
            n += 1;
        };
        let _ = self.client.create_workspace(&name);
        // Born here → looked at here; the daemon subscribes us on create.
        self.subs_pref.mark_seen(&name);
        self.save_arrangement_local();
        if !self.extra_workspaces.contains(&name) {
            self.extra_workspaces.push(name.clone());
        }
        self.ensure_workspace_at_bottom(&name);
        self.selected_workspace = Some(name.clone());
        // Empty circle: don't keep a foreign active_slug — that would route
        // focus to a pane in another workspace after rename finishes.
        self.active_slug = None;
        let _ = self.client.set_focus(None, Some(name.clone()));
        // Immediate inline rename — name is known up front. On Enter/Esc,
        // restore_keyboard_focus parks on the app root so ctrl+shift+n works
        // without an intervening click.
        self.start_rename(RenameTarget::Workspace(name.clone()), &name, window, cx);
        name
    }

    pub(super) fn select_workspace(
        &mut self,
        workspace: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.selected_workspace.as_deref() != Some(workspace);
        // Selecting is looking — clears the `needs` badge. The daemon
        // auto-subscribes on SetFocus, so nothing to ask for here.
        if self.subs_pref.mark_seen(workspace) {
            self.save_arrangement_local();
        }
        // Remember which pane was active in the circle we're leaving.
        if changed {
            // The hovered pane row leaves with its circle, and a row that is
            // gone never reports the pointer leaving it.
            self.rail_hover_pane = None;
            if let (Some(old_ws), Some(slug)) =
                (self.selected_workspace.clone(), self.active_slug.clone())
            {
                if self
                    .panes
                    .iter()
                    .any(|p| p.slug == slug && p.workspace == old_ws)
                {
                    self.workspace_focus.insert(old_ws, slug);
                }
            }
        }
        self.selected_workspace = Some(workspace.to_string());
        // Reveal the selection in the rail. Scrolling alone isn't enough: a
        // circle inside a folded band or cluster has no row to scroll TO, so
        // jumping into one (the sleeping band starts folded) left
        // the rail sitting wherever it was, showing no sign of where you went.
        // Unfold first, then scroll.
        self.reveal_workspace_row(workspace);
        // Selecting a circle clears sticky "done/needs" unread — does NOT bump touch.
        self.workspace_unread.remove(workspace);
        // When entering a circle that was off-screen, zero local revs for its
        // panes so the daemon's full flush can't be dropped as "stale". The
        // daemon also sends FULL frames on workspace change.
        if changed {
            let slugs: Vec<String> = self
                .panes
                .iter()
                .filter(|p| p.workspace == workspace)
                .map(|p| p.slug.clone())
                .collect();
            for slug in slugs {
                if let Some(rt) = self
                    .panes
                    .iter()
                    .find(|p| p.slug == slug)
                    .and_then(|p| p.remote_terminal())
                    .cloned()
                {
                    // Keep last pixels until the full frame lands — only reset
                    // the rev gate, not the cells (avoids a blank flash).
                    rt.update(cx, |t, _| t.open_rev_gate());
                }
            }
        }
        // Invariant: workspace with panes always has an active pane.
        // Keep current active if it's already in this workspace; else restore
        // remembered / first tiled / any.
        let restore = self
            .active_slug
            .clone()
            .filter(|s| {
                self.panes
                    .iter()
                    .any(|p| p.slug == *s && p.workspace == workspace)
            })
            .or_else(|| self.preferred_pane_in_workspace(workspace));
        if let Some(slug) = restore {
            if self.active_slug.as_deref() != Some(slug.as_str()) {
                self.set_active(&slug, window, cx);
                return;
            }
            let _ = self
                .client
                .set_focus(Some(slug), Some(workspace.to_string()));
        } else {
            // Empty workspace — no pane to activate. Park keyboard focus on
            // the app root: the previously focused terminal's view is still
            // ALIVE (its pane just isn't rendered in this circle), so GPUI
            // happily keeps focus on a handle that is no longer in the
            // dispatch tree — capture never runs and ctrl+page stops working
            // until you click. `window.focused()` is Some there, so
            // ensure_keyboard_focus's None-recovery can't save us either.
            self.active_slug = None;
            self.pending_focus = None;
            let fh = self.focus_handle.clone();
            window.focus(&fh, cx);
            let _ = self.client.set_focus(None, Some(workspace.to_string()));
        }
        self.persist(cx);
        cx.notify();
    }

    /// Fold the current selection into the back/forward history. Called once
    /// per render.
    ///
    /// The selection moves from a dozen places — a rail click, ctrl+page, the
    /// jump palette, clicking a pane that lives in another circle
    /// (`set_active` sets it directly), a `ctl`
    /// spawn pulling this window across — and the daemon can move it without
    /// this window asking. Watching the value catches all of them; asking
    /// every caller to remember would catch the ones I thought of today.
    pub(super) fn sync_nav_history(&mut self) {
        if let Some(sel) = self.selected_workspace.clone() {
            self.nav.visit(&sel);
        }
    }

    /// Mouse back button: the circle you were in before this one.
    pub(super) fn nav_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let known = self.known_workspace_names();
        let Some(ws) = self.nav.back(|w| known.contains(w)) else {
            return;
        };
        self.nav_to(&ws, "back", window, cx);
    }

    /// Mouse forward button: undo a back.
    pub(super) fn nav_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let known = self.known_workspace_names();
        let Some(ws) = self.nav.forward(|w| known.contains(w)) else {
            return;
        };
        self.nav_to(&ws, "forward", window, cx);
    }

    fn nav_to(&mut self, ws: &str, dir: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.client.log_event(
            "human",
            Some(ws),
            None,
            "workspace_selected",
            format!("navigated {dir} to workspace '{ws}'"),
        );
        // The cursor already points at `ws`, so the render-time observer reads
        // this as "still where the history says we are" and leaves the forward
        // half alone.
        self.select_workspace(ws, window, cx);
    }

    /// Cycle the selected workspace in sidebar order. `delta` is +1 (next /
    /// PageDown) or -1 (prev / PageUp). Wraps. Focuses a pane in the target
    /// workspace when one exists so keyboard goes there.
    pub(super) fn cycle_workspace(
        &mut self,
        delta: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Parked circles are deliberately out of the rotation — that's the
        // point of folding them away. Cycle EXACTLY the list the sidebar shows,
        // read live at each press (owner decision 2026-08-02: pageup/down
        // must always correspond to what the left sidebar displays — no
        // snapshots, no alternate orders).
        let list = self.visible_workspaces();
        if list.is_empty() {
            return;
        }
        let cur = self
            .selected_workspace
            .as_deref()
            .and_then(|w| list.iter().position(|x| x == w))
            .unwrap_or(0);
        let n = list.len() as i32;
        let next = (cur as i32 + delta).rem_euclid(n) as usize;
        let ws = list[next].clone();
        if self.selected_workspace.as_deref() == Some(ws.as_str()) {
            return;
        }
        self.client.log_event(
            "human",
            Some(&ws),
            None,
            "workspace_selected",
            format!("cycled to workspace '{ws}'"),
        );
        // Restores last active pane for `ws` (or first tiled/any).
        self.select_workspace(&ws, window, cx);
    }

    /// Jump to the rail's TOP row (ctrl+shift+home / ctrl+shift+1) — whatever
    /// the sort says is first: the first pinned circle if anything is pinned,
    /// else the top of active (a working agent, else most recent), and so on
    /// down the bands.
    pub(super) fn select_top_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_nth_workspace(0, window, cx);
    }

    /// Jump to rail row `idx` (0-based; ctrl+shift+1..9). Reads the same live
    /// list ctrl+page walks, so the row you count with your eye is the row you
    /// get — folded bands aren't on screen and aren't counted. Out of range is
    /// a no-op rather than a clamp: pressing 7 on a five-circle rail means you
    /// were counting a rail that isn't there, and landing somewhere arbitrary
    /// is worse than nothing happening.
    pub(super) fn select_nth_workspace(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.visible_workspaces().into_iter().nth(idx) else {
            return;
        };
        if self.selected_workspace.as_deref() == Some(ws.as_str()) {
            return;
        }
        self.client.log_event(
            "human",
            Some(&ws),
            None,
            "workspace_selected",
            format!("jumped to rail row {} — '{ws}'", idx + 1),
        );
        self.select_workspace(&ws, window, cx);
    }

    pub(super) fn move_to_workspace(
        &mut self,
        slug: &str,
        workspace: &str,
        cx: &mut Context<Self>,
    ) {
        // Append into target workspace (no before-slug) — same path as drag
        // onto a workspace header, so order persists via the daemon.
        self.reorder_pane(slug, workspace, None, cx);
    }

    /// Kill every pane in a workspace, then drop the workspace itself.
    /// Sidebar banish × click. Banishing kills every pane's PTY and nothing
    /// brings them back, so a bare click only *arms* the ×; the kill needs a
    /// second click on the same circle inside `BANISH_ARM`.
    pub(super) fn banish_click(
        &mut self,
        workspace: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if banish_arm_live(self.banish_armed.as_ref(), workspace, Instant::now()) {
            self.banish_armed = None;
            self.kill_workspace(workspace, window, cx);
            return;
        }
        // Arming a different circle replaces the old arm, so only ever one ×
        // is hot.
        self.banish_armed = Some((workspace.to_string(), Instant::now()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BANISH_ARM).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |app: &mut SeanceApp, cx| {
                    // Only clear a *stale* arm — a click that re-armed while
                    // this timer slept owns the × now.
                    if app
                        .banish_armed
                        .as_ref()
                        .is_some_and(|(_, at)| at.elapsed() >= BANISH_ARM)
                    {
                        app.banish_armed = None;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Sidebar pane-row × click: arms like [`Self::banish_click`], and the
    /// second click inside `BANISH_ARM` kills the pane (the last pane takes
    /// its circle with it, as ctrl+shift+w does).
    pub(super) fn pane_kill_click(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if banish_arm_live(self.pane_kill_armed.as_ref(), slug, Instant::now()) {
            self.pane_kill_armed = None;
            self.kill_pane(slug, window, cx);
            return;
        }
        self.pane_kill_armed = Some((slug.to_string(), Instant::now()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BANISH_ARM).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |app: &mut SeanceApp, cx| {
                    if app
                        .pane_kill_armed
                        .as_ref()
                        .is_some_and(|(_, at)| at.elapsed() >= BANISH_ARM)
                    {
                        app.pane_kill_armed = None;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    pub(super) fn kill_workspace(
        &mut self,
        workspace: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Banishing the ACTIVE circle: select the neighbor below (above when
        // last) in sidebar order — not the daemon's arbitrary first-pane
        // fallback — so the human lands somewhere predictable.
        if self.selected_workspace.as_deref() == Some(workspace) {
            let order = self.visible_workspaces();
            if let Some(idx) = order.iter().position(|w| w == workspace) {
                let neighbor = order
                    .get(idx + 1)
                    .or_else(|| idx.checked_sub(1).and_then(|j| order.get(j)))
                    .cloned();
                if let Some(n) = neighbor {
                    let _ = self.client.kill_workspace(workspace);
                    self.select_workspace(&n, window, cx);
                    cx.notify();
                    return;
                }
            }
        }
        let _ = self.client.kill_workspace(workspace);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seance_core::util::recency_rank;

    /// Drive the history the way the app does: every selection change is
    /// folded in by the render-time observer, including the ones our own
    /// back/forward caused.
    fn observe(nav: &mut NavHistory, ws: &str) {
        nav.visit(ws);
    }

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn back_then_forward_returns_to_where_you_left() {
        let mut nav = NavHistory::default();
        for ws in ["a", "b", "c"] {
            observe(&mut nav, ws);
        }
        assert_eq!(nav.back(all).as_deref(), Some("b"));
        observe(&mut nav, "b");
        assert_eq!(nav.back(all).as_deref(), Some("a"));
        observe(&mut nav, "a");
        assert_eq!(nav.back(all), None, "nothing before the first circle");
        assert_eq!(nav.forward(all).as_deref(), Some("b"));
        observe(&mut nav, "b");
        assert_eq!(nav.forward(all).as_deref(), Some("c"));
        observe(&mut nav, "c");
        assert_eq!(nav.forward(all), None);
    }

    #[test]
    fn a_fresh_visit_drops_the_forward_half() {
        let mut nav = NavHistory::default();
        for ws in ["a", "b", "c"] {
            observe(&mut nav, ws);
        }
        nav.back(all);
        observe(&mut nav, "b");
        // Now go somewhere new instead of forward — "c" is gone.
        observe(&mut nav, "d");
        assert_eq!(nav.forward(all), None);
        assert_eq!(nav.back(all).as_deref(), Some("b"));
    }

    #[test]
    fn walking_back_is_not_itself_a_new_visit() {
        // The regression the cursor-first invariant exists to prevent: if the
        // observer treated our own step as a fresh navigation it would
        // truncate, and forward would be dead after one back.
        let mut nav = NavHistory::default();
        for ws in ["a", "b"] {
            observe(&mut nav, ws);
        }
        let target = nav.back(all).unwrap();
        observe(&mut nav, &target);
        assert_eq!(nav.forward(all).as_deref(), Some("b"));
    }

    #[test]
    fn reselecting_the_same_circle_records_nothing() {
        let mut nav = NavHistory::default();
        observe(&mut nav, "a");
        observe(&mut nav, "a");
        observe(&mut nav, "a");
        assert_eq!(nav.back(all), None);
    }

    #[test]
    fn revisiting_a_circle_is_a_new_entry_not_a_jump() {
        let mut nav = NavHistory::default();
        for ws in ["a", "b", "a"] {
            observe(&mut nav, ws);
        }
        assert_eq!(nav.back(all).as_deref(), Some("b"));
        observe(&mut nav, "b");
        assert_eq!(nav.back(all).as_deref(), Some("a"));
    }

    #[test]
    fn banished_circles_are_stepped_over_both_ways() {
        let mut nav = NavHistory::default();
        for ws in ["a", "gone", "c"] {
            observe(&mut nav, ws);
        }
        let alive = |w: &str| w != "gone";
        assert_eq!(nav.back(alive).as_deref(), Some("a"));
        observe(&mut nav, "a");
        assert_eq!(nav.forward(alive).as_deref(), Some("c"));
    }

    #[test]
    fn history_is_bounded_and_keeps_the_newest() {
        let mut nav = NavHistory::default();
        for i in 0..NAV_HISTORY_MAX + 10 {
            observe(&mut nav, &format!("c{i}"));
        }
        assert_eq!(nav.entries.len(), NAV_HISTORY_MAX);
        assert_eq!(
            nav.entries.last().map(String::as_str),
            Some(format!("c{}", NAV_HISTORY_MAX + 9).as_str())
        );
        // The cursor survives the trim — back still walks, forward doesn't
        // wander off the end.
        assert_eq!(
            nav.back(all).as_deref(),
            Some(format!("c{}", NAV_HISTORY_MAX + 8).as_str())
        );
    }

    #[test]
    fn banish_arm_is_per_circle_and_expires() {
        let now = Instant::now();
        let armed = Some(("circle-7".to_string(), now));

        // Live only for the circle actually armed, and only inside the window.
        assert!(banish_arm_live(armed.as_ref(), "circle-7", now));
        assert!(banish_arm_live(
            armed.as_ref(),
            "circle-7",
            now + BANISH_ARM - Duration::from_millis(1)
        ));
        assert!(!banish_arm_live(armed.as_ref(), "circle-2", now));

        // Dead on the boundary, and a never-clicked × is never live — so the
        // second click can only land on the circle the first one named.
        assert!(!banish_arm_live(
            armed.as_ref(),
            "circle-7",
            now + BANISH_ARM
        ));
        assert!(!banish_arm_live(None, "circle-7", now));
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A pill dropped on another takes its place, whichever way it moved.
    #[test]
    fn a_dropped_circle_takes_the_targets_place() {
        let order = names(&["a", "b", "c", "d"]);
        let right = drop_circle(&order, "a", "c").unwrap();
        assert_eq!(right, (names(&["b", "c", "a", "d"]), "d".to_string()));
        let left = drop_circle(&order, "d", "b").unwrap();
        assert_eq!(left, (names(&["a", "d", "b", "c"]), "b".to_string()));
        let last = drop_circle(&order, "b", "d").unwrap();
        assert_eq!(last, (names(&["a", "c", "d", "b"]), String::new()));
        assert_eq!(drop_circle(&order, "b", "b"), None);
        assert_eq!(drop_circle(&order, "b", "gone"), None);
    }

    /// Stage shows one space's circles; dragging right lands just after the
    /// target there even when the next circle overall lives in another space.
    #[test]
    fn a_drop_keeps_its_place_among_another_spaces_circles() {
        let order = names(&["a", "x", "b", "y", "c"]);
        let (out, before) = drop_circle(&order, "a", "b").unwrap();
        assert_eq!(before, "y");
        let mine: Vec<_> = out
            .iter()
            .filter(|w| !["x", "y"].contains(&w.as_str()))
            .collect();
        assert_eq!(mine, ["b", "a", "c"]);
    }

    /// Monotonic across every boundary — a non-monotonic rank would reorder
    /// rows as they age.
    #[test]
    fn recency_rank_is_monotonic_across_bucket_edges() {
        let edges = [
            0, 599_999, 600_000, 3_599_999, 3_600_000, 86_399_999, 86_400_000,
        ];
        let ranks: Vec<u64> = edges.iter().map(|ms| recency_rank(*ms)).collect();
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "{ranks:?}");
        assert_eq!(recency_rank(0), recency_rank(599_999), "under 10m all ties");
        assert!(recency_rank(599_999) < recency_rank(600_000), "9m < 10m");
        assert!(
            recency_rank(3_599_999) < recency_rank(3_600_000),
            "59m < 1h"
        );
    }
}
