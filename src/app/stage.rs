//! Stage: one circle's lead up front. A strip of circle pills on top, the
//! needs-you bar, the live lead terminal with its Doing / Next / Waiting on
//! line, the crew as cards, the circle's files, and a footer of Claude plan
//! usage and machine health.
//!
//! Stage replaces the rail and grid rather than drawing over them, so the
//! pane on stage is the one place its terminal view renders: keys and esc
//! reach it like any focused pane. The pane on stage is the window's active
//! pane when that is a terminal in the circle, else the circle's lead.
//! The lead log, health report and usage file are daemon-side files read through the fs
//! bridge on a background task, never from render.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{div, prelude::*, px, relative, AnyElement, Context, Hsla, SharedString, Window};
use gpui_component::{input::Input, menu::ContextMenuExt as _, ActiveTheme as _, StyledExt as _};

use crate::gui_client::fs_not_found;
use crate::pane::{Pane, PaneBody};
use crate::runtime::snapshot::GridSnapshot;
use crate::theme::SeancePalette;

use super::actions::{ActDeleteSpace, ActRenameSpace, ActSpaceDefault};
use super::stage_parse::{
    circle_folder, compacted_while_idle, crew_order, done_chips, ellipsize, filed_label, fit_pills,
    is_bare_shell, jump_ring, last_output_lines, latest_finish, latest_finish_at, lead_log_path,
    next_waiting, pane_host, parse_health, parse_lead_log, parse_usage, pill_width, reply_repeats,
    reset_in, usage_now, CrewBand, Health, HealthLevel, LeadLine, Usage,
};
use super::util::{pane_menu, tip, tip_s, DragPill, DraggedCircle};
use super::views::ViewMode;
use super::workspaces::{banish_arm_live, rel_label, WorkspaceAttention};
use super::{PaletteMode, RenameTarget, SeanceApp};

/// The lead log changes a few times an hour; the health check every 2.5h.
const LEAD_LOG_EVERY: Duration = Duration::from_secs(30);
const HEALTH_EVERY: Duration = Duration::from_secs(60);
/// Plan usage moves with every Claude reply.
/// Under the 15 s repaint so an idle Stage doesn't skip every other tick.
const USAGE_EVERY: Duration = Duration::from_secs(10);
/// A reading older than this is hidden rather than trusted.
const USAGE_STALE_SECS: u64 = 6 * 3600;
/// Width of one usage bar in the footer.
const USAGE_BAR_W: f32 = 64.;
/// Below this the crew column folds behind a toggle so the terminal keeps
/// its width.
const CREW_FOLD_BELOW: f32 = 1000.;
/// Stage and Wall's row of space tabs, and Stage's row above it.
pub(super) const SPACE_ROW_H: f32 = 30.;
/// Lines a done chip's peek card shows of the finish note.
const PEEK_LINES: usize = 12;
/// Bottom rows read for a crew card's last line: enough to clear a TUI's
/// input box without walking the whole screen.
const LAST_LINE_ROWS: usize = 30;

/// Stage's per-window state. Everything here is derived or cached; nothing
/// is saved.
#[derive(Default)]
pub(super) struct StageState {
    /// circle slug → its lead, from the daemon's State.
    pub(super) leads: HashMap<String, String>,
    /// Put the circle's lead up on the next frame: set on coming onto
    /// Stage and by a circle pill. Other moves (a jump, ctrl+page, Open)
    /// keep the pane they land on.
    pub(super) want_lead: bool,
    /// "+N more" opened: the circles that don't fit sit in a row under the
    /// strip until it's clicked again.
    pub(super) pills_open: bool,
    /// This frame's height of that row, so the jump palette drops below it.
    pub(super) pills_row_h: f32,
    /// The circle "+ circle" just made, while its name is being typed:
    /// Enter opens its first pane.
    pub(super) new_circle: Option<String>,
    /// (circle, its newest lead log entry).
    lead_log: Option<(String, Option<LeadLine>)>,
    lead_log_asked: Option<(String, Instant)>,
    lead_log_busy: bool,
    /// The health report: Stage's footer, and the Wall's too.
    pub(super) health: Option<Health>,
    health_asked: Option<Instant>,
    health_busy: bool,
    /// Claude plan usage: the footer's bars, and the Wall's.
    pub(super) usage: Option<Usage>,
    usage_asked: Option<Instant>,
    usage_busy: bool,
    /// Narrow windows: the crew column is open.
    crew_open: bool,
    /// circle → the file tab Stage shows. Kept apart from the dock's pick
    /// so Stage's tiled files never overwrite Classic's dock tab.
    file_tab: HashMap<String, String>,
    /// Crew cards' last output lines.
    last_lines: LineCache,
    /// The next-waiting key's last pick (circle, pane) and the items that
    /// followed it in the ring then.
    jumped: Option<(String, String)>,
    jump_after: Vec<(String, String)>,
    /// The done chip whose peek card is open.
    pub(super) peek: Option<Peek>,
    /// The space tabs' scroll, and the space it last brought into view.
    space_tabs: gpui::ScrollHandle,
    space_tabs_shown: Option<String>,
}

/// A done chip's peek card: its circle, the pane it shows and that pane's
/// pad, read once on opening (None until the read lands).
pub(super) struct Peek {
    circle: String,
    pane: String,
    pad: Option<String>,
}

/// pane slug → (grid rev, its last few output lines), so a card or tile
/// reads its grid only when the grid changed.
#[derive(Default)]
pub(super) struct LineCache(HashMap<String, (u64, Vec<String>)>);

impl LineCache {
    pub(super) fn lines(&mut self, slug: &str, snap: &GridSnapshot) -> &[String] {
        let stale = self.0.get(slug).is_none_or(|(rev, _)| *rev != snap.rev);
        if stale {
            let lines = last_output_lines(&bottom_rows(snap, LAST_LINE_ROWS), 6);
            self.0.insert(slug.to_string(), (snap.rev, lines));
        }
        self.0.get(slug).map_or(&[], |(_, l)| l.as_slice())
    }

    pub(super) fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.0.retain(|s, _| keep(s));
    }
}

/// What a pane is doing, for its header or card.
pub(super) struct PaneState {
    pub(super) label: String,
    pub(super) color: Hsla,
    band: CrewBand,
    /// A shell with nothing in it, or a sleeping pane: the dashed card.
    quiet: bool,
}

/// One chip in the needs-you bar.
pub(super) struct Waiting {
    pub(super) circle: String,
    pub(super) pane: Option<String>,
    pub(super) what: String,
    ask: Option<(String, Vec<String>)>,
    /// A pane that finished while you were elsewhere, not one waiting.
    done: bool,
    /// A pane you marked unread to come back to.
    later: bool,
}

impl SeanceApp {
    /// Coming onto Stage shows the selected circle's lead.
    pub(super) fn stage_enter(&mut self) {
        self.stage.want_lead = true;
    }

    /// Repaint now and then on Stage and Wall: elapsed labels move and the
    /// lead logs, health report, usage and Today feed come due.
    pub(super) fn start_stage_ticker(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_secs(15))
                .await;
            let Some(this) = this.upgrade() else { break };
            this.update(cx, |app: &mut SeanceApp, cx| {
                if matches!(app.view.mode, ViewMode::Stage | ViewMode::Wall) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The circle's lead: the daemon's pick while it is a terminal here,
    /// else the circle's first terminal pane.
    pub(super) fn stage_lead(&self, ws: &str) -> Option<String> {
        let ok = |p: &&Pane| p.workspace == ws && stageable(p);
        self.stage
            .leads
            .get(ws)
            .and_then(|l| self.panes.iter().filter(ok).find(|p| &p.slug == l))
            .or_else(|| self.panes.iter().find(ok))
            .map(|p| p.slug.clone())
    }

    /// The pane Stage shows for the selected circle.
    pub(super) fn stage_pane(&self) -> Option<String> {
        let ws = self.selected_workspace.as_deref()?;
        self.active_slug
            .as_deref()
            .and_then(|a| {
                self.panes
                    .iter()
                    .find(|p| p.slug == a && p.workspace == ws && stageable(p))
            })
            .map(|p| p.slug.clone())
            .or_else(|| self.stage_lead(ws))
    }

    /// Keep the active pane on what Stage shows, so keys land there: the
    /// lead when asked for, and whenever the active pane is one Stage can't
    /// show (a file, a popped-out pane).
    pub(super) fn stage_sync(&mut self) {
        let Some(ws) = self.selected_workspace.clone() else {
            return;
        };
        // A jump to one of the circle's files opens its tab here.
        if let Some(a) = self.active_slug.as_deref() {
            if self.panes.iter().any(|p| {
                p.slug == a && p.workspace == ws && matches!(p.body, PaneBody::File { .. })
            }) {
                self.stage.file_tab.insert(ws.clone(), a.to_string());
            }
        }
        let target = if std::mem::take(&mut self.stage.want_lead) {
            self.stage_lead(&ws)
        } else {
            self.stage_pane()
        };
        let Some(target) = target else {
            return;
        };
        if self.active_slug.as_deref() != Some(target.as_str()) {
            self.active_slug = Some(target.clone());
            self.workspace_focus.insert(ws.clone(), target.clone());
            let _ = self.client.set_focus(Some(target.clone()), Some(ws));
            self.pending_focus = Some(target);
        }
    }

    /// Leave the Wall for Stage on `ws`: its lead, or `pane` when given
    /// (a needs-you tile's Open puts the pane that asked up front).
    pub(super) fn focus_on_stage(
        &mut self,
        ws: &str,
        pane: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_workspace.as_deref() != Some(ws) {
            self.select_workspace(ws, window, cx);
        }
        self.set_view(ViewMode::Stage, cx);
        if let Some(slug) = pane {
            self.stage.want_lead = false;
            self.set_active(slug, window, cx);
        }
    }

    /// Put a pane on stage, switching circle first when it lives elsewhere.
    fn stage_show(&mut self, ws: &str, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_workspace.as_deref() != Some(ws) {
            self.select_workspace(ws, window, cx);
        }
        self.set_active(slug, window, cx);
    }

    /// A chip's Open: the pane on stage, and a done chip's circle seen even
    /// when it was already selected.
    fn open_chip(
        &mut self,
        ws: &str,
        slug: &str,
        done: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stage.peek = None;
        if done {
            self.workspace_unread.remove(ws);
        }
        self.stage_show(ws, slug, window, cx);
    }

    /// ctrl+shift+i: open the next pane waiting on you, in every circle and
    /// space, in the needs-you bar's order. Classic and Stage open it like
    /// the chip's Open; the Wall only selects its circle's tile.
    pub(super) fn jump_next_waiting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (chips, _) = self.waiting_list();
        let pairs: Vec<(&str, Option<&str>)> = chips
            .iter()
            .map(|w| (w.circle.as_str(), w.pane.as_deref()))
            .collect();
        let wall = self.view.mode == ViewMode::Wall;
        let mut ring = jump_ring(&pairs);
        // The Wall opens circles, not panes: one stop per circle.
        if wall {
            let mut seen: Vec<String> = Vec::new();
            ring.retain(|(c, _)| {
                let first = !seen.contains(c);
                seen.push(c.clone());
                first
            });
        }
        let cur = match (&self.selected_workspace, &self.active_slug) {
            (Some(ws), _) if wall => ring.iter().find(|(c, _)| c == ws).cloned(),
            (Some(ws), Some(slug)) => Some((ws.clone(), slug.clone())),
            _ => None,
        };
        let Some(i) = next_waiting(
            &ring,
            self.stage.jumped.as_ref(),
            &self.stage.jump_after,
            cur.as_ref(),
        ) else {
            return;
        };
        let (ws, slug) = ring[i].clone();
        self.stage.jumped = Some(ring[i].clone());
        self.stage.jump_after = ring[i + 1..].to_vec();
        if wall {
            // The Wall shows one space (All aside): make it the circle's so
            // its tile is there (a select only moves the rail for circles
            // placed by hand).
            self.rail_space = Some(self.subs_pref.spaces.space_of(&ws).to_string());
            if self.selected_workspace.as_deref() != Some(ws.as_str()) {
                self.select_workspace(&ws, window, cx);
            }
        } else {
            // A zoom on Classic would hide the pane: zoom it instead.
            if self.zoomed_slug.is_some() && self.view.mode == ViewMode::Classic {
                self.zoomed_slug = Some(slug.clone());
            }
            self.open_chip(&ws, &slug, false, window, cx);
        }
        cx.notify();
    }

    /// A done chip's body: open its peek card, or close it when it's the
    /// one open. The pad is read now rather than on the cache's next tick.
    /// A done chip's body: open its peek card, or close it when it's the
    /// one open. The pad is read once here, not kept on the cache's poll.
    fn toggle_peek(&mut self, ws: &str, slug: &str, cx: &mut Context<Self>) {
        if self
            .stage
            .peek
            .as_ref()
            .is_some_and(|p| p.circle == ws && p.pane == slug)
        {
            self.stage.peek = None;
            cx.notify();
            return;
        }
        self.stage.peek = Some(Peek {
            circle: ws.to_string(),
            pane: slug.to_string(),
            pad: None,
        });
        let path = Self::scratch_path(slug, "md");
        let client = Arc::clone(&self.client);
        let (ws, slug) = (ws.to_string(), slug.to_string());
        cx.spawn(async move |this, cx| {
            // A missing pad reads as empty: "no finish note".
            let pad = cx
                .background_executor()
                .spawn(async move { client.fs_read_string(&path).map(|(s, _)| s) })
                .await
                .unwrap_or_default();
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                if let Some(p) = app.stage.peek.as_mut() {
                    if p.circle == ws && p.pane == slug {
                        p.pad = Some(pad);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// ctrl+shift+page on Stage: step through the panes Stage can show, the
    /// lead and its crew, in pane order. Files and popped panes would only
    /// bounce back to the lead.
    pub(super) fn stage_cycle(
        &mut self,
        ws: &str,
        delta: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let list: Vec<String> = self
            .panes
            .iter()
            .filter(|p| p.workspace == ws && stageable(p))
            .map(|p| p.slug.clone())
            .collect();
        if list.len() < 2 {
            return;
        }
        let cur = self
            .stage_pane()
            .and_then(|s| list.iter().position(|x| *x == s))
            .unwrap_or(0);
        let next = (cur as i32 + delta).rem_euclid(list.len() as i32) as usize;
        self.set_active(&list[next], window, cx);
    }

    pub(super) fn open_jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = PaletteMode::Jump {
            query: String::new(),
            selected: 0,
        };
        let fh = self.focus_handle.clone();
        window.focus(&fh, cx);
        cx.notify();
    }

    /// Ask the daemon for the lead log, health report and usage when they're due.
    /// Cheap when nothing is: a few comparisons per frame.
    fn stage_feeds(&mut self, cx: &mut Context<Self>) {
        self.health_feed(cx);
        let conf = crate::term_config::get();
        if let (Some(map), Some(ws)) = (
            conf.seance_circle_map.as_ref(),
            self.selected_workspace.as_ref(),
        ) {
            let due = !self.stage.lead_log_busy
                && self
                    .stage
                    .lead_log_asked
                    .as_ref()
                    .is_none_or(|(c, at)| c != ws || at.elapsed() >= LEAD_LOG_EVERY);
            if due {
                let (map, ws) = (map.clone(), ws.clone());
                self.stage.lead_log_busy = true;
                self.stage.lead_log_asked = Some((ws.clone(), Instant::now()));
                let client = Arc::clone(&self.client);
                let circle = ws.clone();
                cx.spawn(async move |this, cx| {
                    let line = cx
                        .background_executor()
                        .spawn(async move {
                            match client.fs_read_string(&map) {
                                Ok((text, _)) => read_lead_line(&client, &map, &text, &circle),
                                Err(e) if fs_not_found(&e) => Some(None),
                                Err(_) => None,
                            }
                        })
                        .await;
                    let Some(this) = this.upgrade() else { return };
                    this.update(cx, |app: &mut SeanceApp, cx| {
                        app.stage.lead_log_busy = false;
                        // The circle changed mid-read: repaint so its own
                        // read starts now, not on the next tick.
                        if app.selected_workspace.as_ref() != Some(&ws) {
                            cx.notify();
                        }
                        // A failed read keeps the card as it was.
                        let Some(line) = line else { return };
                        let fresh = Some((ws, line));
                        if app.stage.lead_log != fresh {
                            app.stage.lead_log = fresh;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// Re-read the health report and the usage file when they're due (Stage's
    /// footer, the Wall's feed and footer).
    pub(super) fn health_feed(&mut self, cx: &mut Context<Self>) {
        let conf = crate::term_config::get();
        if let Some(path) = conf.seance_health_file.as_ref() {
            let due = !self.stage.health_busy
                && self
                    .stage
                    .health_asked
                    .is_none_or(|at| at.elapsed() >= HEALTH_EVERY);
            if due {
                let path = path.clone();
                self.stage.health_busy = true;
                self.stage.health_asked = Some(Instant::now());
                let client = Arc::clone(&self.client);
                cx.spawn(async move |this, cx| {
                    let health = cx
                        .background_executor()
                        .spawn(async move {
                            match client.fs_read_string(&path) {
                                Ok((text, _)) => Some(parse_health(&text)),
                                Err(e) if fs_not_found(&e) => Some(None),
                                Err(_) => None,
                            }
                        })
                        .await;
                    let Some(this) = this.upgrade() else { return };
                    this.update(cx, |app: &mut SeanceApp, cx| {
                        app.stage.health_busy = false;
                        // A failed read keeps the last report on screen.
                        let Some(health) = health else { return };
                        if app.stage.health != health {
                            app.stage.health = health;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
        if let Some(path) = conf.seance_usage_file.as_ref() {
            let due = !self.stage.usage_busy
                && self
                    .stage
                    .usage_asked
                    .is_none_or(|at| at.elapsed() >= USAGE_EVERY);
            if due {
                let path = path.clone();
                self.stage.usage_busy = true;
                self.stage.usage_asked = Some(Instant::now());
                let client = Arc::clone(&self.client);
                cx.spawn(async move |this, cx| {
                    let usage = cx
                        .background_executor()
                        .spawn(async move {
                            match client.fs_read_string(&path) {
                                Ok((text, _)) => Some(parse_usage(&text)),
                                Err(e) if fs_not_found(&e) => Some(None),
                                Err(_) => None,
                            }
                        })
                        .await;
                    let Some(this) = this.upgrade() else { return };
                    this.update(cx, |app: &mut SeanceApp, cx| {
                        app.stage.usage_busy = false;
                        // A failed read keeps the last reading on screen.
                        let Some(usage) = usage else { return };
                        if app.stage.usage != usage {
                            app.stage.usage = usage;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    pub(super) fn pane_state(&self, p: &Pane, cx: &Context<Self>) -> PaneState {
        let st = |label: &str, color, band, quiet| PaneState {
            label: label.to_string(),
            color,
            band,
            quiet,
        };
        if self.owners.get(&p.slug).is_some_and(|o| o.exited) {
            return st("✕ exited", SeancePalette::danger(), CrewBand::Quiet, false);
        }
        if p.asleep {
            return st(
                "☾ asleep",
                SeancePalette::text_faint(),
                CrewBand::Quiet,
                true,
            );
        }
        match self.pane_attention(&p.slug) {
            Some(WorkspaceAttention::NeedsHuman) => {
                return st(
                    "● needs you",
                    SeancePalette::violet(),
                    CrewBand::Needs,
                    false,
                );
            }
            Some(WorkspaceAttention::Working) => {
                let label = match self.busy_since.get(&p.slug) {
                    Some(t) => format!("◐ working · {}", rel_label(t.elapsed().as_millis() as u64)),
                    None => "◐ working".into(),
                };
                return st(&label, SeancePalette::flame(), CrewBand::Working, false);
            }
            _ => {}
        }
        let done = self.done_unseen.contains(&p.slug)
            || self
                .statuses
                .get(&p.slug)
                .is_some_and(|s| s.state == "done");
        if done {
            return st("✓ done", SeancePalette::success(), CrewBand::Done, false);
        }
        if self.subs_pref.later.contains(&p.slug) {
            return st(
                "● unread",
                WorkspaceAttention::Later.color(),
                CrewBand::Done,
                false,
            );
        }
        let title = p.title(cx);
        if is_bare_shell(&p.command, title.as_deref(), false) {
            return st("shell", SeancePalette::text_faint(), CrewBand::Quiet, true);
        }
        st("idle", SeancePalette::text_faint(), CrewBand::Idle, false)
    }

    /// Everything waiting on you, in every circle: panes that say they need
    /// you and questions agents asked.
    pub(super) fn stage_waiting(&self) -> Vec<Waiting> {
        let mut out = Vec::new();
        for p in &self.panes {
            let ask = self
                .asks
                .iter()
                .find(|a| a.answer.is_none() && a.from == p.slug);
            let status = self
                .statuses
                .get(&p.slug)
                .filter(|s| matches!(s.state.as_str(), "needs-human" | "blocked" | "risky"));
            if ask.is_none() && status.is_none() {
                continue;
            }
            let what = match (ask, status) {
                (Some(a), _) => format!("{} asks: {}", p.name, a.question),
                (None, Some(s)) => match s.note.as_deref().filter(|n| !n.is_empty()) {
                    Some(n) => format!("{}: {n}", p.name),
                    None if s.state == "blocked" => format!("{} is blocked", p.name),
                    None => format!("{} needs you", p.name),
                },
                (None, None) => continue,
            };
            out.push(Waiting {
                circle: p.workspace.clone(),
                pane: Some(p.slug.clone()),
                what,
                ask: None,
                done: false,
                later: false,
            });
        }
        // Questions from outside any pane (a ctl ask) have no pane to open,
        // so their answers ride on the chip.
        for a in self.asks.iter().filter(|a| a.answer.is_none()) {
            if self.panes.iter().any(|p| p.slug == a.from) {
                continue;
            }
            let choices = if a.choices.is_empty() {
                vec!["ok".to_string(), "no".to_string()]
            } else {
                a.choices.clone()
            };
            out.push(Waiting {
                circle: a.workspace.clone().unwrap_or_default(),
                pane: None,
                what: format!("{} asks: {}", a.from, a.question),
                ask: Some((a.id.clone(), choices)),
                done: false,
                later: false,
            });
        }
        out
    }

    /// Circles with a finished pane you haven't seen (their sticky unread
    /// is done), as chips after the waiting ones. Viewing the circle clears
    /// its unread, and with it the chip. The circle you're on gets a chip
    /// per finished pane instead, until you open that pane.
    fn stage_done(&self) -> Vec<Waiting> {
        let mut out = self.stage_done_elsewhere();
        let Some(here) = self.selected_workspace.as_deref() else {
            return out;
        };
        out.extend(
            self.panes
                .iter()
                .filter(|p| p.workspace == here && self.finished_unseen(&p.slug))
                .map(|p| Waiting {
                    circle: p.workspace.clone(),
                    pane: Some(p.slug.clone()),
                    what: p.name.clone(),
                    ask: None,
                    done: true,
                    later: false,
                }),
        );
        out
    }

    /// A done chip that names a pane that finished, not the lead standing in
    /// for a circle's leftover mark: only that pane can be snoozed.
    fn real_done(&self, slug: &str) -> bool {
        self.done_unseen.contains(slug)
            || self.statuses.get(slug).is_some_and(|s| s.state == "done")
    }

    /// Finished out of sight and not back at work or asking since.
    fn finished_unseen(&self, slug: &str) -> bool {
        self.done_unseen.contains(slug)
            && !matches!(
                self.pane_attention(slug),
                Some(WorkspaceAttention::Working | WorkspaceAttention::NeedsHuman)
            )
    }

    fn stage_done_elsewhere(&self) -> Vec<Waiting> {
        // The circle you're on: paths that select without select_workspace
        // can leave its unread behind.
        let selected = self.selected_workspace.as_deref();
        let unread: Vec<&str> = self
            .workspace_unread
            .iter()
            .filter(|(ws, a)| {
                matches!(a, WorkspaceAttention::Done) && Some(ws.as_str()) != selected
            })
            .map(|(ws, _)| ws.as_str())
            .collect();
        if unread.is_empty() {
            return Vec::new();
        }
        let in_order = |keep: &dyn Fn(&Pane) -> bool| -> Vec<(&str, &str)> {
            self.panes
                .iter()
                .filter(|p| keep(p))
                .map(|p| (p.workspace.as_str(), p.slug.as_str()))
                .collect()
        };
        let mut unseen = in_order(&|p| self.done_unseen.contains(&p.slug));
        let mut status_done = in_order(&|p| {
            self.statuses
                .get(&p.slug)
                .is_some_and(|s| s.state == "done")
        });
        // A circle's chip names its first done pane: not a snoozed one while
        // another is there.
        unseen.sort_by_key(|(_, s)| self.snoozed(s));
        status_done.sort_by_key(|(_, s)| self.snoozed(s));
        done_chips(&self.workspaces(), &unread, &unseen, &status_done)
            .into_iter()
            .map(|(circle, pane)| {
                let name = pane
                    .as_deref()
                    .and_then(|s| self.panes.iter().find(|p| p.slug == s))
                    .map(|p| p.name.clone());
                Waiting {
                    // No done pane: Open puts the lead up.
                    pane: pane.or_else(|| self.stage_lead(&circle)),
                    what: name.unwrap_or_else(|| "finished".into()),
                    circle,
                    ask: None,
                    done: true,
                    later: false,
                }
            })
            .collect()
    }

    /// Panes you marked unread, in rail order, any circle: a chip each,
    /// after the done ones, until you open the pane or clear it. A pane
    /// that already has a chip keeps just that one.
    fn stage_later(&self, chips: &[Waiting]) -> Vec<Waiting> {
        let order = self.workspaces();
        let mut out: Vec<Waiting> = self
            .panes
            .iter()
            .filter(|p| self.subs_pref.later.contains(&p.slug))
            .filter(|p| {
                !chips
                    .iter()
                    .any(|w| w.pane.as_deref() == Some(p.slug.as_str()))
            })
            .map(|p| Waiting {
                circle: p.workspace.clone(),
                pane: Some(p.slug.clone()),
                what: p.name.clone(),
                ask: None,
                done: false,
                later: true,
            })
            .collect();
        out.sort_by_key(|w| order.iter().position(|c| *c == w.circle));
        out
    }

    /// The needs-you bar's chips in order: waiting, done, then unread. The
    /// bar and the next-waiting key both read this. Snoozed panes' chips
    /// are left out; the second value counts those panes.
    pub(super) fn waiting_list(&self) -> (Vec<Waiting>, usize) {
        let mut out = self.stage_waiting();
        out.extend(self.stage_done());
        let later = self.stage_later(&out);
        out.extend(later);
        let mut snoozed: Vec<String> = Vec::new();
        out.retain(|w| match w.pane.as_deref().filter(|p| self.snoozed(p)) {
            Some(p) => {
                if !snoozed.iter().any(|s| s == p) {
                    snoozed.push(p.to_string());
                }
                false
            }
            None => true,
        });
        (out, snoozed.len())
    }

    /// The whole Stage layout, in place of the rail and grid.
    pub(super) fn render_stage(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.stage_feeds(cx);
        let width: f32 = window.viewport_size().width.into();
        let ws = self.selected_workspace.clone();
        let on_stage = self.stage_pane();
        let lead = ws.as_deref().and_then(|w| self.stage_lead(w));
        let fold = width < CREW_FOLD_BELOW;
        let crew_w = if width >= 1200. { 330. } else { 260. };
        let show_crew = !fold || self.stage.crew_open;

        let head = self.render_stage_head(cx);
        let tabs = self.render_space_tabs("stage", cx);
        let top = self.render_stage_top(width, cx);
        let (chips, snoozed) = self.waiting_list();
        // A peek card whose chip went (seen, its pane closed, its circle
        // gone or selected) closes with it.
        let peeked = self.stage.peek.as_ref().and_then(|p| {
            chips
                .iter()
                .find(|w| w.done && w.circle == p.circle && w.pane.as_deref() == Some(&p.pane))
                .map(|w| w.pane.clone())
        });
        let peek = match peeked {
            Some(open) => self.render_stage_peek(open, cx),
            None => {
                self.stage.peek = None;
                None
            }
        };
        let needs = self.render_stage_needs(chips, snoozed, cx);
        let lead_el = self.render_stage_lead(on_stage.as_deref(), lead.as_deref(), fold, cx);
        let crew_el = show_crew.then(|| {
            let crew = self.render_stage_crew(on_stage.as_deref(), crew_w, cx);
            if !fold {
                return crew;
            }
            // Folded and opened: the crew floats over the terminal's right
            // edge rather than narrowing it (a resize would reflow the PTY).
            div()
                .id("stage-crew-over")
                .absolute()
                .top(px(10.))
                .right(px(10.))
                .bottom(px(10.))
                .p(px(8.))
                .rounded(px(10.))
                .bg(SeancePalette::bg())
                .border_1()
                .border_color(SeancePalette::border())
                .occlude()
                .child(crew)
                .into_any_element()
        });
        let footer = self.render_stage_footer();

        div()
            .id("stage")
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(SeancePalette::bg())
            .child(self.render_daemon_error_bar(cx))
            .child(head)
            .children(tabs)
            .child(top)
            .children(needs)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .gap(px(10.))
                    .p(px(10.))
                    .child(lead_el)
                    .children(crew_el)
                    .children(peek),
            )
            .children(footer)
            .into_any_element()
    }

    /// Stage's top row: the ✦ mark, the layout switch beside it and the AFK
    /// chip at the right. Drawn with one space or many.
    fn render_stage_head(&self, cx: &Context<Self>) -> AnyElement {
        div()
            .flex_none()
            .h(px(SPACE_ROW_H))
            .px(px(14.))
            .flex()
            .items_center()
            .gap(px(14.))
            .bg(SeancePalette::bg_elevated())
            .border_b_1()
            .border_color(SeancePalette::border())
            .child(self.render_seance_mark("stage", cx))
            .child(
                div()
                    .flex_none()
                    .w(px(SWITCH_W))
                    .child(self.render_view_switch("strip", cx)),
            )
            .child(div().flex_1())
            .children(self.render_afk_chip("stage", cx))
            .into_any_element()
    }

    /// Circle pills, "+ circle" and the jump box. With "+N more" open, the
    /// circles that don't fit get a row of their own underneath.
    fn render_stage_top(&mut self, width: f32, cx: &Context<Self>) -> AnyElement {
        let rename = self.render_rename_field();
        let renaming = rename.is_some();
        let selected = self.selected_workspace.clone();
        let space = self.shown_space();
        let circles = self.subs_pref.spaces.circles_in(&space, &self.workspaces());
        let roomy = width >= 900.;
        let jump_w = jump_box_w(roomy);
        let add_w = 76.;
        // Strip padding, the jump box, "+ circle" and the gaps.
        let avail = width - 28. - jump_w - 14. * 2. - add_w - 6.;
        let labels: Vec<String> = circles.iter().map(|c| self.workspace_label(c)).collect();
        let widths: Vec<f32> = labels.iter().map(|l| pill_width(l)).collect();
        let sel_idx = selected
            .as_ref()
            .and_then(|s| circles.iter().position(|c| c == s));
        let shown = fit_pills(&widths, avail, 74., 6., sel_idx);
        let hidden: Vec<usize> = (0..circles.len()).filter(|i| !shown.contains(i)).collect();
        if hidden.is_empty() {
            self.stage.pills_open = false;
        }
        let open = self.stage.pills_open && !renaming;

        // The hidden circles' row: wraps when they don't fit on one line.
        // Its height is estimated for the palette, which drops below it.
        let row_w = width - 28.;
        let row_lines = if open {
            let mut lines = 1.;
            let mut x = 0.;
            for &i in &hidden {
                if x > 0. && x + widths[i] > row_w {
                    lines += 1.;
                    x = 0.;
                }
                x += widths[i] + 6.;
            }
            lines
        } else {
            0.
        };
        self.stage.pills_row_h = if open {
            13. + row_lines * 30. + (row_lines - 1.) * 6.
        } else {
            0.
        };
        let pill = |i: usize| {
            let ws = circles[i].clone();
            let on = sel_idx == Some(i);
            let glyph = self.circle_glyph(&ws);
            div()
                .id(SharedString::from(format!("stage-pill-{ws}")))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(7.))
                .px(px(12.))
                .py(px(5.))
                .rounded_full()
                .text_xs()
                .cursor_pointer()
                .when(on, |d| {
                    d.bg(SeancePalette::surface())
                        .text_color(SeancePalette::text())
                        .font_medium()
                })
                .when(!on, |d| {
                    d.text_color(SeancePalette::text_dim())
                        .hover(|s| s.text_color(SeancePalette::text()))
                })
                // Drag a pill onto another to take its place in the order.
                .on_drag(DraggedCircle { slug: ws.clone() }, {
                    let label = format!("◈ {}", labels[i]);
                    move |_, _, _, cx| {
                        cx.new(|_| DragPill {
                            label: label.clone(),
                        })
                    }
                })
                .drag_over::<DraggedCircle>(|style, _, _, _| style.bg(SeancePalette::violet_dim()))
                .on_drop(cx.listener({
                    let ws = ws.clone();
                    move |this, drag: &DraggedCircle, _, cx| {
                        this.reorder_circle(&drag.slug, &ws);
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_workspace(&ws, window, cx);
                    this.stage.want_lead = true;
                    cx.notify();
                }))
                .child(glyph)
                .child(labels[i].clone())
        };
        let pills: Vec<_> = shown.iter().map(|&i| pill(i)).collect();
        let more = (!hidden.is_empty()).then(|| {
            let names = hidden
                .iter()
                .map(|&i| labels[i].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            div()
                .id("stage-pill-more")
                .flex_none()
                .px(px(10.))
                .py(px(5.))
                .rounded_full()
                .border_1()
                .border_dashed()
                .border_color(if open {
                    SeancePalette::flame_dim()
                } else {
                    SeancePalette::border()
                })
                .text_xs()
                .text_color(if open {
                    SeancePalette::text()
                } else {
                    SeancePalette::text_faint()
                })
                .cursor_pointer()
                .hover(|s| s.text_color(SeancePalette::text()))
                .tooltip(tip_s(if open {
                    "hide them again".to_string()
                } else {
                    format!("show {names}")
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.stage.pills_open = !this.stage.pills_open;
                    cx.notify();
                }))
                .child(if open {
                    "less".to_string()
                } else {
                    format!("+{} more", hidden.len())
                })
        });
        let add = div()
            .id("stage-add-circle")
            .flex_none()
            .w(px(add_w))
            .flex()
            .justify_center()
            .py(px(5.))
            .rounded_full()
            .border_1()
            .border_dashed()
            .border_color(SeancePalette::border())
            .text_xs()
            .text_color(SeancePalette::text_faint())
            .cursor_pointer()
            .hover(|s| s.text_color(SeancePalette::text()))
            .tooltip(tip("new circle in this space"))
            .on_click(cx.listener(|this, _, window, cx| {
                // New circles land in the rail's space; make that this one.
                this.rail_space = Some(this.shown_space());
                let name = this.create_workspace(window, cx);
                this.stage.new_circle = Some(name);
            }))
            .child("+ circle");
        // Over the top of what's below, so the lead terminal keeps its size
        // (a resize would reflow the PTY on every click).
        let row = open.then(|| {
            div()
                .id("stage-pills-row")
                .absolute()
                .top(px(STRIP_H))
                .left_0()
                .right_0()
                .occlude()
                .px(px(14.))
                .py(px(6.))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .bg(SeancePalette::bg_elevated())
                .border_b_1()
                .border_color(SeancePalette::border())
                .children(hidden.iter().map(|&i| pill(i)))
        });

        let strip = div()
            .flex_none()
            .h(px(STRIP_H))
            .px(px(14.))
            .flex()
            .items_center()
            .gap(px(14.))
            .bg(SeancePalette::bg_elevated())
            .border_b_1()
            .border_color(SeancePalette::border())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children(rename)
                    .when(!renaming, |d| d.children(pills).children(more).child(add)),
            )
            .child(render_jump_box("stage-jump", roomy, cx));
        div()
            .flex_none()
            .relative()
            .child(strip)
            .children(row)
            .into_any_element()
    }

    /// The ✦ seance mark; a click opens the connected guis and theme menu.
    pub(super) fn render_seance_mark(&self, key: &str, cx: &Context<Self>) -> AnyElement {
        div()
            .id(SharedString::from(format!("{key}-mark")))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(7.))
            .px_1()
            .rounded_md()
            .text_sm()
            .font_semibold()
            .cursor_pointer()
            .hover(|s| s.bg(SeancePalette::surface()))
            .tooltip(tip("connected guis · theme"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.gui_menu_open = !this.gui_menu_open;
                cx.notify();
            }))
            .child(div().text_color(SeancePalette::flame()).child("✦"))
            .child(div().text_color(SeancePalette::text()).child("seance"))
            .into_any_element()
    }

    /// Stage and Wall draw their row of space tabs when it would hold two
    /// or more: a tab per space with circles, plus the one on show even if
    /// empty, so an empty space always has a way out. Classic's rail lists
    /// every space.
    pub(super) fn space_tabs_shown(&self) -> bool {
        if self.view.mode == ViewMode::Classic {
            return false;
        }
        let ordered = self.workspaces();
        let shown = self.shown_space();
        let spaces = &self.subs_pref.spaces;
        spaces
            .list
            .iter()
            .filter(|sp| sp.id == shown || !spaces.circles_in(&sp.id, &ordered).is_empty())
            .nth(1)
            .is_some()
    }

    /// What the space tabs take off the top, for overlays placed below it.
    pub(super) fn space_tabs_h(&self) -> f32 {
        if self.space_tabs_shown() {
            SPACE_ROW_H
        } else {
            0.
        }
    }

    /// Stage and Wall's space row: a tab per space with the mark its rail
    /// row has (needs you, working, done), the one on show underlined.
    /// Empty spaces get no tab unless on show. Wall leads with an All tab.
    /// Scrolls sideways when the tabs don't fit. Wall's row leads with the ✦
    /// mark and ends with the AFK chip; Stage has them in the row above.
    /// None when there would be fewer than two.
    pub(super) fn render_space_tabs(
        &mut self,
        key: &str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.space_tabs_shown() {
            return None;
        }
        let shown = self.shown_space();
        let spaces = &self.subs_pref.spaces;
        let ordered = self.workspaces();
        // Wall leads with All; on it, no space's tab is lit.
        let wall = self.view.mode == ViewMode::Wall;
        let all = self.wall_all();
        // No space id has a `*`.
        let mark = if all {
            "*all".to_string()
        } else {
            shown.clone()
        };
        // A new space on show scrolls its tab into view; the index counts
        // tabs, and empty spaces have none.
        if self.stage.space_tabs_shown.as_deref() != Some(mark.as_str()) {
            let i = if all {
                Some(0)
            } else {
                spaces
                    .list
                    .iter()
                    .filter(|sp| sp.id == shown || !spaces.circles_in(&sp.id, &ordered).is_empty())
                    .position(|sp| sp.id == shown)
                    .map(|i| i + usize::from(wall))
            };
            if let Some(i) = i {
                self.stage.space_tabs.scroll_to_item(i);
            }
            self.stage.space_tabs_shown = Some(mark);
        }
        let steps = if cfg!(target_os = "macos") {
            "⌘⌥↑ / ⌘⌥↓ steps"
        } else {
            "ctrl+alt+page up / down steps"
        };
        let tabs = spaces.list.iter().filter_map(|sp| {
            let n = spaces.circles_in(&sp.id, &ordered).len();
            let on = sp.id == shown;
            if n == 0 && !on {
                return None;
            }
            let on = on && !all;
            let id = sp.id.clone();
            let id_menu = sp.id.clone();
            let is_default = spaces.default_id() == sp.id;
            let count = match n {
                0 => "no circles".to_string(),
                1 => "1 circle".to_string(),
                n => format!("{n} circles"),
            };
            Some(
                div()
                    .id(SharedString::from(format!("{key}-space-{}", sp.id)))
                    .tooltip(tip_s(format!("{} · {count} ({steps})", sp.name)))
                    .flex_none()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .border_b_2()
                    .border_color(if on {
                        SeancePalette::flame()
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if on {
                        SeancePalette::text()
                    } else if n > 0 {
                        SeancePalette::text_dim()
                    } else {
                        SeancePalette::text_faint()
                    })
                    .when(n > 0 && !on, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.text_color(SeancePalette::text()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_space(&id, window, cx);
                            }))
                    })
                    .child(sp.name.clone())
                    .when_some(self.space_mark(&sp.id), |d, (glyph, color)| {
                        d.child(div().text_color(color).child(glyph))
                    })
                    // The rail's space menu, minus what only the rail can
                    // show (new, reorder, banish), so a space can be closed
                    // without leaving Stage or Wall.
                    .context_menu(move |menu, _, _| {
                        let m =
                            menu.menu("rename space", Box::new(ActRenameSpace(id_menu.clone())));
                        if is_default {
                            return m;
                        }
                        m.menu("make default", Box::new(ActSpaceDefault(id_menu.clone())))
                            .separator()
                            .menu(
                                "delete space (circles go to the default)",
                                Box::new(ActDeleteSpace(id_menu.clone())),
                            )
                    }),
            )
        });
        let all_tab = wall.then(|| {
            div()
                .id(SharedString::from(format!("{key}-space-all")))
                .tooltip(tip("All · every circle from every space"))
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .border_b_2()
                .border_color(if all {
                    SeancePalette::flame()
                } else {
                    gpui::transparent_black()
                })
                .text_color(if all {
                    SeancePalette::text()
                } else {
                    SeancePalette::text_dim()
                })
                .when(!all, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.text_color(SeancePalette::text()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_wall_all(true);
                            cx.notify();
                        }))
                })
                .child("All")
        });
        let wall_ends = key == "wall";
        Some(
            div()
                .flex_none()
                .h(px(SPACE_ROW_H))
                .px(px(14.))
                .flex()
                .items_center()
                .gap(px(14.))
                .bg(SeancePalette::bg_elevated())
                .border_b_1()
                .border_color(SeancePalette::border())
                .when(wall_ends, |d| d.child(self.render_seance_mark(key, cx)))
                .child(
                    div()
                        .id(SharedString::from(format!("{key}-spaces")))
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .gap(px(16.))
                        .overflow_x_scroll()
                        .track_scroll(&self.stage.space_tabs)
                        .text_xs()
                        .children(all_tab)
                        .children(tabs),
                )
                .when(wall_ends, |d| d.children(self.render_afk_chip(key, cx)))
                .into_any_element(),
        )
    }

    /// The rename field, for layouts with no rail to hold it (Stage, Wall):
    /// a focused field outside the frame would strand the keyboard.
    pub(super) fn render_rename_field(&self) -> Option<AnyElement> {
        let (target, input) = self.renaming.as_ref()?;
        let what = match target {
            RenameTarget::Pane(_) => "rename pane",
            RenameTarget::Workspace(_) => "rename circle",
            RenameTarget::Space(_) => "rename space",
        };
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(SeancePalette::text_faint())
                        .child(what),
                )
                .child(div().w(px(260.)).child(Input::new(input)))
                .into_any_element(),
        )
    }

    /// A pill's status mark: needs you, working, done, exited or idle.
    fn circle_glyph(&self, ws: &str) -> AnyElement {
        let exited = self
            .panes
            .iter()
            .any(|p| p.workspace == ws && self.owners.get(&p.slug).is_some_and(|o| o.exited));
        match self.workspace_attention_cx(ws) {
            Some(WorkspaceAttention::NeedsHuman) => div()
                .flex_none()
                .size(px(14.))
                .rounded_full()
                .bg(SeancePalette::violet().opacity(0.22))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(SeancePalette::violet()),
                )
                .into_any_element(),
            Some(WorkspaceAttention::Working) => glyph("◐", SeancePalette::flame()),
            Some(WorkspaceAttention::Done) => glyph("✓", SeancePalette::success()),
            Some(WorkspaceAttention::Later) => glyph("●", WorkspaceAttention::Later.color()),
            None if exited => glyph("✕", SeancePalette::danger()),
            None => div()
                .flex_none()
                .size(px(6.))
                .rounded_full()
                .bg(SeancePalette::text_faint())
                .into_any_element(),
        }
    }

    /// "Needs you · N" and a chip per waiting pane, every circle, then
    /// "Done · N" and a chip per finished one. Hidden when there are none.
    fn render_stage_needs(
        &self,
        waiting: Vec<Waiting>,
        snoozed: usize,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if waiting.is_empty() && snoozed == 0 {
            return None;
        }
        let only_snoozed = waiting.is_empty();
        let n_done = waiting.iter().filter(|w| w.done).count();
        let n_later = waiting.iter().filter(|w| w.later).count();
        let n = waiting.len() - n_done - n_later;
        let label = [("Needs you", n), ("Done", n_done), ("Unread", n_later)]
            .iter()
            .filter(|(_, k)| *k > 0)
            .map(|(what, k)| format!("{what} · {k}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let accent = if n > 0 {
            SeancePalette::violet()
        } else if n_done > 0 {
            SeancePalette::success()
        } else {
            WorkspaceAttention::Later.color()
        };
        let peek = self
            .stage
            .peek
            .as_ref()
            .map(|p| (p.circle.as_str(), p.pane.as_str()));
        let chips = waiting.into_iter().enumerate().map(|(i, w)| {
            let peeking =
                w.done && peek == Some((w.circle.as_str(), w.pane.as_deref().unwrap_or("")));
            let circle_label = if w.circle.is_empty() {
                String::new()
            } else {
                self.workspace_label(&w.circle)
            };
            let mut chip = div()
                .id(SharedString::from(format!("stage-wait-{i}")))
                .flex_none()
                .max_w(px(460.))
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(10.))
                .pr(px(6.))
                .py(px(4.))
                .rounded_lg()
                .bg(SeancePalette::bg_elevated())
                .border_1()
                .border_color(if peeking {
                    SeancePalette::success()
                } else {
                    SeancePalette::border()
                })
                .when(!circle_label.is_empty(), |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_color(SeancePalette::text_dim())
                            .child(circle_label),
                    )
                })
                .when(w.done, |d| d.child(glyph("✓", SeancePalette::success())))
                .when(w.later, |d| {
                    d.child(glyph("●", WorkspaceAttention::Later.color()))
                })
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(SeancePalette::text())
                        .child(ellipsize(&w.what, 70)),
                );
            if let Some(slug) = w.pane {
                let slug_for_snooze = slug.clone();
                let ws = w.circle.clone();
                let done = w.done;
                if done {
                    let (ws, slug) = (ws.clone(), slug.clone());
                    chip = chip
                        .cursor_pointer()
                        .tooltip(tip("peek at its finish note"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_peek(&ws, &slug, cx);
                        }));
                }
                let read = w.later.then(|| slug.clone());
                chip = chip.child(
                    chip_button("Open", true)
                        .id(SharedString::from(format!("stage-wait-open-{i}")))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_chip(&ws, &slug, done, window, cx);
                        })),
                );
                if !w.later && (!done || self.real_done(&slug_for_snooze)) {
                    let slug = slug_for_snooze;
                    chip = chip.child(
                        chip_button("Snooze 1h", false)
                            .id(SharedString::from(format!("stage-wait-snooze-{i}")))
                            .tooltip(tip("hide this for an hour: no chip, banner or ding"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.snooze(&slug);
                                cx.notify();
                            })),
                    );
                }
                if let Some(slug) = read {
                    chip = chip.child(
                        chip_button("Read", false)
                            .id(SharedString::from(format!("stage-wait-read-{i}")))
                            .tooltip(tip("clear the unread mark without opening"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.mark_unread(&slug, false);
                                cx.notify();
                            })),
                    );
                }
            }
            if let Some((id, choices)) = w.ask {
                for (j, choice) in choices.into_iter().enumerate() {
                    let id = id.clone();
                    chip = chip.child(
                        chip_button(&choice, false)
                            .id(SharedString::from(format!("stage-wait-{i}-{j}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer_ask(&id, choice.clone(), cx);
                            })),
                    );
                }
            }
            chip
        });
        Some(
            div()
                .id("stage-needs")
                .flex_none()
                .h(px(42.))
                .px(px(14.))
                .flex()
                .items_center()
                .gap(px(10.))
                .overflow_x_scroll()
                .text_xs()
                .when(!only_snoozed, |d| d.bg(accent.opacity(0.12)))
                .border_b_1()
                .border_color(SeancePalette::border())
                .when(!only_snoozed, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .font_semibold()
                            .text_color(accent)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(SeancePalette::text_faint())
                            .child(if cfg!(target_os = "macos") {
                                "⌘⇧I next"
                            } else {
                                "ctrl+shift+i next"
                            }),
                    )
                })
                .children(chips)
                .when(snoozed > 0, |d| {
                    d.child(
                        div()
                            .id("stage-snoozed")
                            .flex_none()
                            .cursor_pointer()
                            .text_color(SeancePalette::text_faint())
                            .hover(|s| s.text_color(SeancePalette::text()))
                            .tooltip(tip("bring them back now"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.unsnooze_all();
                                cx.notify();
                            }))
                            .child(format!("{snoozed} snoozed")),
                    )
                })
                .into_any_element(),
        )
    }

    /// A done chip's peek card, over the top of the terminal so the PTY
    /// keeps its size: the pane's latest finish note and when it was
    /// filed, its last reply's first line, Open and Seen. It
    /// takes no focus; keys stay with the pane on stage.
    /// `open` is the chip's pane now, for Open.
    fn render_stage_peek(&self, open: Option<String>, cx: &Context<Self>) -> Option<AnyElement> {
        let peek = self.stage.peek.as_ref()?;
        let ws = peek.circle.clone();
        let read = peek.pad.is_some();
        let note = peek
            .pad
            .as_deref()
            .and_then(|pad| latest_finish(pad, PEEK_LINES));
        let filed = peek.pad.as_deref().and_then(latest_finish_at).map(|t| {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            let now = crate::events::fmt_time(now_ms);
            filed_label(&t, &now[..5])
        });
        // The first line of the pane's last reply (its status note), so a
        // pane that kept talking after its last report doesn't look stuck
        // on it. Working or asking replaces that note.
        let reply = self
            .statuses
            .get(&peek.pane)
            .and_then(|s| s.note.as_deref())
            .and_then(|n| n.strip_prefix("turn ended: "))
            .filter(|r| {
                note.as_ref()
                    .and_then(|n| n.first())
                    .is_none_or(|first| !reply_repeats(r, first))
            })
            .map(str::to_string);
        let name = self
            .panes
            .iter()
            .find(|p| p.slug == peek.pane)
            .map_or_else(|| "finished".to_string(), |p| p.name.clone());
        let body = match note {
            Some(lines) => div()
                .flex()
                .flex_col()
                .text_color(SeancePalette::text())
                .children(lines.into_iter().map(|l| {
                    // An empty div has no height: keep blank lines.
                    div().min_h(px(14.)).child(l)
                })),
            None => div()
                .text_color(SeancePalette::text_faint())
                .child(if read { "no finish note" } else { "reading…" }),
        };
        let seen_ws = ws.clone();
        let seen_pane = peek.pane.clone();
        let mut buttons = div().flex().gap(px(6.)).justify_end();
        if let Some(slug) = open {
            let ws = ws.clone();
            buttons = buttons.child(chip_button("Open", true).id("stage-peek-open").on_click(
                cx.listener(move |this, _, window, cx| {
                    this.open_chip(&ws, &slug, true, window, cx);
                }),
            ));
        }
        if self.real_done(&peek.pane) {
            let snooze_pane = peek.pane.clone();
            buttons = buttons.child(
                chip_button("Snooze 1h", false)
                    .id("stage-peek-snooze")
                    .tooltip(tip("hide this for an hour: no chip, banner or ding"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.stage.peek = None;
                        this.snooze(&snooze_pane);
                        cx.notify();
                    })),
            );
        }
        buttons = buttons.child(
            chip_button("Seen", false)
                .id("stage-peek-seen")
                .tooltip(tip("clear the done mark without opening"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.stage.peek = None;
                    this.workspace_unread.remove(&seen_ws);
                    this.done_unseen.remove(&seen_pane);
                    crate::desktop_notify::clear_pane(&seen_pane);
                    cx.notify();
                })),
        );
        div()
            .id("stage-peek")
            .absolute()
            .top(px(4.))
            .left(px(14.))
            .w(px(560.))
            .max_w(relative(0.9))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(10.))
            .bg(SeancePalette::bg_elevated())
            .border_1()
            .border_color(SeancePalette::success())
            .text_xs()
            .occlude()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(glyph("✓", SeancePalette::success()))
                    .child(
                        div()
                            .text_color(SeancePalette::text_dim())
                            .child(self.workspace_label(&ws)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_semibold()
                            .text_color(SeancePalette::text())
                            .child(name),
                    )
                    .children(filed.map(|t| {
                        div()
                            .flex_none()
                            .ml_auto()
                            .text_color(SeancePalette::text_faint())
                            .child(t)
                    })),
            )
            .children(reply.map(|r| {
                div()
                    .flex()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .text_color(SeancePalette::text_faint())
                            .child("Last reply"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(SeancePalette::text())
                            .child(r),
                    )
            }))
            .child(body)
            .child(buttons)
            .into_any_element()
            .into()
    }

    /// The pane on stage: its header, the lead log line, the live terminal.
    fn render_stage_lead(
        &self,
        on_stage: Option<&str>,
        lead: Option<&str>,
        fold: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let frame = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(SeancePalette::border())
            .overflow_hidden();
        let Some(pane) = on_stage.and_then(|s| self.panes.iter().find(|p| p.slug == s)) else {
            return frame
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(SeancePalette::text_dim())
                        .child("No terminal in this circle yet"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(SeancePalette::text_faint())
                        .child("+ worker starts one"),
                )
                .into_any_element();
        };
        let is_lead = lead == Some(pane.slug.as_str());
        let (name, host) = pane_host(&pane.name, &pane.command);
        let state = self.pane_state(pane, cx);
        let crew_n = self
            .panes
            .iter()
            .filter(|p| p.workspace == pane.workspace && stageable(p) && p.slug != pane.slug)
            .count();
        let ws = pane.workspace.clone();
        let line = self
            .stage
            .lead_log
            .as_ref()
            .filter(|(c, _)| *c == ws)
            .and_then(|(_, l)| l.clone());

        let mut row1 = div()
            .flex()
            .items_center()
            .gap(px(9.))
            .text_sm()
            .min_w_0()
            .when(is_lead, |d| {
                d.child(super::sprite::render(
                    &super::sprite::for_circle(&pane.workspace),
                    2.5,
                    self.workspace_has_working_agent(&pane.workspace),
                ))
            })
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(SeancePalette::text_faint())
                    .child(if is_lead { "LEAD" } else { "ON STAGE" }),
            )
            .child(
                div()
                    .id("stage-title")
                    .min_w_0()
                    .truncate()
                    .font_semibold()
                    .text_color(SeancePalette::text())
                    .tooltip(tip_s(format!("{} · double-click to rename", pane.name)))
                    .on_click(cx.listener({
                        let slug = pane.slug.clone();
                        let full = pane.name.clone();
                        move |this, event: &gpui::ClickEvent, window, cx| {
                            if event.click_count() == 2 {
                                this.start_rename(
                                    RenameTarget::Pane(slug.clone()),
                                    &full,
                                    window,
                                    cx,
                                );
                            }
                        }
                    }))
                    .child(name.to_string()),
            )
            .children(host.map(host_chip));
        if !is_lead {
            if let Some(lead) = lead.map(str::to_string) {
                row1 = row1.child(
                    chip_button("← back to lead", false)
                        .id("stage-back-to-lead")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_active(&lead, window, cx);
                        })),
                );
            }
        }
        // Come back to this one later: stays marked until you leave and
        // open it again.
        let later = self.subs_pref.later.contains(&pane.slug);
        let unread_slug = pane.slug.clone();
        row1 = row1.child(
            chip_button(if later { "mark read" } else { "mark unread" }, false)
                .id("stage-unread")
                .tooltip(tip(if later {
                    "clear the unread mark"
                } else {
                    "keep a chip for this pane until you next open it"
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mark_unread(&unread_slug, !later);
                    cx.notify();
                })),
        );
        if pane.asleep {
            let ws = ws.clone();
            row1 = row1.child(chip_button("awaken", true).id("stage-awaken").on_click(
                cx.listener(move |this, _, window, cx| {
                    this.wake_workspace_focused(&ws, window, cx);
                }),
            ));
        }
        row1 = row1.child(
            div()
                .ml_auto()
                .flex_none()
                .text_xs()
                .text_color(state.color)
                .child(state.label),
        );
        if fold {
            let open = self.stage.crew_open;
            row1 = row1.child(
                chip_button(
                    &format!("crew · {crew_n} {}", if open { "▸" } else { "◂" }),
                    false,
                )
                .id("stage-crew-toggle")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.stage.crew_open = !this.stage.crew_open;
                    cx.notify();
                })),
            );
        }
        row1 = row1.child(self.stage_kill_button(&pane.slug, is_lead, cx));

        // Crew that finished out of sight: one line on the lead's card,
        // the newest, until you open that pane.
        let crew_line = is_lead
            .then(|| {
                let mut done: Vec<(&Pane, Instant)> = self
                    .panes
                    .iter()
                    .filter(|c| c.workspace == ws && c.slug != pane.slug)
                    .filter(|c| self.finished_unseen(&c.slug))
                    .filter_map(|c| Some((c, *self.crew_done.get(&c.slug)?)))
                    .collect();
                done.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
                let more = done.len().saturating_sub(1);
                done.first().map(|(c, at)| (*c, *at, more))
            })
            .flatten()
            .map(|(crew, at, more)| {
                let p = &crew.slug;
                let said = self
                    .statuses
                    .get(p)
                    .and_then(|s| s.note.as_deref())
                    .and_then(|n| n.strip_prefix("turn ended: "))
                    .map(|n| format!(" · {n}"))
                    .unwrap_or_default();
                let ago = rel_label(at.elapsed().as_millis() as u64);
                let when = if ago == "now" {
                    ago
                } else {
                    format!("{ago} ago")
                };
                let tail = if more > 0 {
                    format!("  (+{more} more)")
                } else {
                    String::new()
                };
                let slug = p.clone();
                div()
                    .id("stage-crew-done")
                    .flex()
                    .gap_1()
                    .min_w_0()
                    .text_xs()
                    .cursor_pointer()
                    .tooltip(tip("open it"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_active(&slug, window, cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .text_color(SeancePalette::success())
                            .child("✓ Crew"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(SeancePalette::text_dim())
                            .child(format!("{} finished {when}{said}{tail}", crew.name)),
                    )
            });

        let row2 = line.map(|l| {
            let field = |key: &'static str, value: Option<String>, accent: bool| {
                value.map(|v| {
                    div()
                        .min_w_0()
                        .flex_shrink(1.)
                        .flex()
                        .gap_1()
                        .child(
                            div()
                                .flex_none()
                                .text_color(SeancePalette::text_faint())
                                .child(key),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(if accent {
                                    SeancePalette::violet()
                                } else {
                                    SeancePalette::text_dim()
                                })
                                .child(v),
                        )
                })
            };
            div()
                .flex()
                .gap(px(18.))
                .min_w_0()
                .overflow_hidden()
                .text_xs()
                .children(field("Doing", l.doing, false))
                .children(field("Next", l.next, false))
                .children(field("Waiting on", l.waiting, true))
        });

        let slug = pane.slug.clone();
        let flipped = self
            .flipped
            .as_ref()
            .filter(|(s, _)| *s == pane.slug)
            .map(|(_, d)| d.clone());
        let body = div()
            .id("stage-body")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.set_active(&slug, window, cx);
                }),
            );
        let body = match flipped {
            Some(notes) => body.bg(SeancePalette::bg_elevated()).child(notes),
            // Asleep: the frozen frame, dimmed like a classic tile.
            None if pane.asleep => body
                .child(
                    div()
                        .size_full()
                        .opacity(0.45)
                        .child(pane.content_element()),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .bg(SeancePalette::bg().opacity(0.35)),
                ),
            None => body.child(pane.content_element()),
        };

        frame
            .child(
                div()
                    .flex_none()
                    .px(px(14.))
                    .py(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .bg(SeancePalette::bg_elevated())
                    .border_b_1()
                    .border_color(SeancePalette::border())
                    .child(row1)
                    .children(row2)
                    .children(crew_line),
            )
            .child(body)
            .into_any_element()
    }

    /// The circle's other terminal panes, in crew order.
    fn stage_crew_slugs(&self, on_stage: Option<&str>, cx: &Context<Self>) -> Vec<String> {
        let Some(ws) = self.selected_workspace.as_deref() else {
            return Vec::new();
        };
        let crew: Vec<&Pane> = self
            .panes
            .iter()
            .filter(|p| p.workspace == ws && stageable(p) && Some(p.slug.as_str()) != on_stage)
            .collect();
        let bands = crew
            .iter()
            .enumerate()
            .map(|(i, p)| (i, self.pane_state(p, cx).band))
            .collect();
        crew_order(bands)
            .into_iter()
            .map(|i| crew[i].slug.clone())
            .collect()
    }

    /// CREW · N with "+ worker", a card per other pane, then FILES.
    fn render_stage_crew(
        &mut self,
        on_stage: Option<&str>,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slugs = self.stage_crew_slugs(on_stage, cx);
        self.stage
            .last_lines
            .retain(|s| slugs.iter().any(|x| x == s));
        let mono = cx.theme().mono_font_family.clone();
        let lead = on_stage
            .and_then(|s| self.panes.iter().find(|p| p.slug == s))
            .and_then(|p| self.stage_lead(&p.workspace));
        let mut cards = Vec::with_capacity(slugs.len());
        for slug in &slugs {
            let Some(idx) = self.panes.iter().position(|p| &p.slug == slug) else {
                continue;
            };
            let pane = &self.panes[idx];
            let state = self.pane_state(pane, cx);
            let (name, host) = pane_host(&pane.name, &pane.command);
            let name = name.to_string();
            let host = host.map(str::to_string);
            let p_slug = pane.slug.clone();
            let snap = pane.remote_terminal().map(|t| t.read(cx).snapshot.clone());
            let last =
                snap.and_then(|snap| self.stage.last_lines.lines(&p_slug, &snap).last().cloned());
            let quiet = state.quiet;
            let click = p_slug.clone();
            let kill = self.stage_kill_button(&p_slug, lead.as_ref() == Some(&p_slug), cx);
            cards.push(
                div()
                    .id(SharedString::from(format!("stage-crew-{p_slug}")))
                    .flex_none()
                    .px(px(12.))
                    .py(px(11.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(SeancePalette::border())
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .cursor_pointer()
                    .when(quiet, |d| d.border_dashed())
                    .when(!quiet, |d| d.bg(SeancePalette::bg_elevated()))
                    .hover(|s| s.border_color(SeancePalette::flame_dim()))
                    .tooltip(tip("put this pane on stage"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_active(&click, window, cx);
                    }))
                    .context_menu({
                        let slug = p_slug.clone();
                        let later = self.subs_pref.later.contains(&p_slug);
                        move |menu, _, _| pane_menu(menu, &slug, later, true)
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_sm()
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .when(!quiet, |d| {
                                        d.font_medium().text_color(SeancePalette::text())
                                    })
                                    .when(quiet, |d| d.text_color(SeancePalette::text_dim()))
                                    .child(name),
                            )
                            .children(host.map(host_chip_owned))
                            .child(
                                div()
                                    .ml_auto()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(state.color)
                                    .child(state.label),
                            )
                            .child(kill),
                    )
                    .children(last.map(|l| {
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(if quiet {
                                SeancePalette::text_faint()
                            } else {
                                SeancePalette::text_dim()
                            })
                            .child(l)
                    }))
                    .into_any_element(),
            );
        }
        let n = cards.len();
        let files = self.render_stage_files(cx);

        div()
            .flex_none()
            .w(px(width))
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(2.))
                    .pt(px(2.))
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child(format!("CREW · {n}"))
                    .child(
                        chip_button("+ worker", false)
                            .id("stage-add-worker")
                            .tooltip(tip("new pane in this circle (ctrl+shift+n)"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.new_default_session(cx);
                            })),
                    ),
            )
            .child(
                // Many workers scroll here; the files keep their share below.
                div()
                    .id("stage-crew-cards")
                    .flex_shrink(1.)
                    .min_h_0()
                    .max_h(relative(if files.is_some() { 0.6 } else { 1.0 }))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .children(cards),
            )
            .children(files)
            .into_any_element()
    }

    /// Stage's ×: the rail's two-click kill, always shown. Armed, its
    /// tooltip says what a second click closes (the circle too, with its
    /// last pane).
    fn stage_kill_button(&self, slug: &str, is_lead: bool, cx: &Context<Self>) -> AnyElement {
        let armed = banish_arm_live(self.pane_kill_armed.as_ref(), slug, Instant::now());
        let last = self.panes.iter().find(|p| p.slug == slug).is_some_and(|p| {
            self.panes
                .iter()
                .filter(|q| q.workspace == p.workspace)
                .count()
                == 1
        });
        let slug = slug.to_string();
        div()
            .id(SharedString::from(format!("stage-kill-{slug}")))
            .flex_none()
            .px_1()
            .rounded_sm()
            .text_xs()
            .text_color(if armed {
                SeancePalette::danger()
            } else {
                SeancePalette::text_faint()
            })
            .when(armed, |d| d.bg(SeancePalette::surface()))
            .hover(|s| {
                s.text_color(SeancePalette::danger())
                    .bg(SeancePalette::surface())
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                // The crew card's own click would put the pane on stage.
                cx.stop_propagation();
                this.pane_kill_click(&slug, window, cx);
            }))
            .tooltip(tip(match (armed, last) {
                (false, _) if is_lead => "close the lead pane",
                (false, _) => "close pane",
                (true, true) => "click again to close this pane and its circle",
                (true, false) => "click again to close this pane",
            }))
            .child(match (armed, is_lead) {
                (false, _) => "×",
                (true, true) => "close lead?",
                (true, false) => "close?",
            })
            .into_any_element()
    }

    /// FILES: the circle's file panes as tabs, the open one below. Shares the
    /// dock's open tab, so both layouts agree on which file is up.
    fn render_stage_files(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let ws = self.selected_workspace.clone()?;
        let files: Vec<&Pane> = self
            .panes
            .iter()
            .filter(|p| {
                p.workspace == ws && p.popped.is_none() && matches!(p.body, PaneBody::File { .. })
            })
            .collect();
        let open = [self.stage.file_tab.get(&ws), self.dock_tab.get(&ws)]
            .into_iter()
            .flatten()
            .find_map(|s| files.iter().find(|p| &p.slug == s))
            .or(files.first())?;
        let open_slug = open.slug.clone();
        let tabs = files.iter().map(|p| {
            let slug = p.slug.clone();
            let on = slug == open_slug;
            let ws = ws.clone();
            div()
                .id(SharedString::from(format!("stage-file-{slug}")))
                .flex_none()
                .pb(px(4.))
                .border_b_2()
                .border_color(if on {
                    SeancePalette::flame()
                } else {
                    gpui::transparent_black()
                })
                .text_color(if on {
                    SeancePalette::text()
                } else {
                    SeancePalette::text_faint()
                })
                .hover(|s| s.text_color(SeancePalette::text()))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.stage.file_tab.insert(ws.clone(), slug.clone());
                    cx.notify();
                }))
                .child(p.name.clone())
        });
        Some(
            div()
                .flex_1()
                .min_h(px(120.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex_none()
                        .mt(px(4.))
                        .px(px(2.))
                        .text_xs()
                        .text_color(SeancePalette::text_faint())
                        .child("FILES"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .rounded(px(10.))
                        .bg(SeancePalette::bg_elevated())
                        .border_1()
                        .border_color(SeancePalette::border())
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .id("stage-file-tabs")
                                .flex_none()
                                .px(px(12.))
                                .pt(px(9.))
                                .flex()
                                .gap(px(12.))
                                .text_xs()
                                .overflow_x_scroll()
                                .children(tabs),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .overflow_hidden()
                                .child(open.content_element()),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The health report's facts, a dot for those that have one: the host, Mac,
    /// leads, and plan while there are no usage bars. None when there is no
    /// report to show.
    pub(super) fn health_facts(&self) -> Option<Vec<(Option<Hsla>, String)>> {
        crate::term_config::get().seance_health_file.as_ref()?;
        let h = self.stage.health.as_ref()?;
        let level = match h.level {
            HealthLevel::Healthy => SeancePalette::success(),
            HealthLevel::Warning => SeancePalette::flame(),
            HealthLevel::Critical => SeancePalette::danger(),
        };
        let host = h.host_label();
        let host_fact = match &h.host_memory {
            Some(m) => format!("{host} {} · {m}", h.status),
            None => format!("{host} {}", h.status),
        };
        let (mac_dot, mac) = match &h.mac {
            Some(m) => (SeancePalette::success(), m.clone()),
            None => (
                SeancePalette::text_faint(),
                "Mac asleep · panes waiting".to_string(),
            ),
        };
        let mut facts = vec![(Some(level), host_fact), (Some(mac_dot), mac)];
        facts.extend(h.leads.clone().map(|l| (None, l)));
        // The usage bars say it better, and live.
        if self.usage_fresh().is_none() {
            facts.extend(h.plan.clone().map(|p| (None, p)));
        }
        Some(facts)
    }

    /// The usage reading and the time now, while it is recent: only Seance's
    /// Claude panes feed the file, so with none running it goes stale and
    /// would claim 0% after its reset.
    fn usage_fresh(&self) -> Option<(Usage, u64)> {
        let u = self.stage.usage?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        (now.saturating_sub(u.at) <= USAGE_STALE_SECS).then_some((u, now))
    }

    /// Claude plan usage, one bar per limit: "5h ▓▓░░ 62% ↻52m". Stage's
    /// footer and the Wall's.
    pub(super) fn usage_items(&self) -> Vec<AnyElement> {
        let Some((u, now)) = self.usage_fresh() else {
            return Vec::new();
        };
        [("5h", u.five_hour), ("7d", u.seven_day)]
            .into_iter()
            .filter_map(|(name, w)| {
                let (used, left) = usage_now(w?, now);
                // The pane status line's thresholds.
                let fill = if used >= 70 {
                    SeancePalette::danger()
                } else if used >= 30 {
                    SeancePalette::flame()
                } else {
                    SeancePalette::success()
                };
                Some(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(name)
                        .child(
                            div()
                                .flex_none()
                                .w(px(USAGE_BAR_W))
                                .h(px(6.))
                                .rounded_full()
                                .bg(SeancePalette::surface())
                                .border_1()
                                .border_color(SeancePalette::border())
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .w(relative(used.min(100) as f32 / 100.))
                                        .bg(fill),
                                ),
                        )
                        .child(
                            div()
                                .text_color(SeancePalette::text_dim())
                                .child(format!("{used}%")),
                        )
                        .when(left > 0, |d| d.child(format!("↻{}", reset_in(left))))
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// Plan usage on the left; the health report's facts and time on the right.
    fn render_stage_footer(&self) -> Option<AnyElement> {
        let usage = self.usage_items();
        let facts = self.health_facts().unwrap_or_default();
        if usage.is_empty() && facts.is_empty() {
            return None;
        }
        let at = self.stage.health.as_ref().and_then(|h| h.at.clone());
        let item = |dot: Option<Hsla>, text: String| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .children(dot.map(|c| div().text_color(c).child("●")))
                .child(text)
        };
        Some(
            div()
                .flex_none()
                .h(px(28.))
                .px(px(14.))
                .flex()
                .items_center()
                .gap(px(18.))
                .overflow_hidden()
                .text_xs()
                .text_color(SeancePalette::text_faint())
                .bg(SeancePalette::bg_elevated())
                .border_t_1()
                .border_color(SeancePalette::border())
                .children(usage)
                // The health side gives way in a narrow window: it shrinks
                // and clips from its left, keeping the report time in view.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .justify_end()
                        .items_center()
                        .gap(px(18.))
                        .children(facts.into_iter().map(|(dot, text)| item(dot, text)))
                        .children(
                            at.map(|at| div().flex_none().child(format!("health report {at}"))),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// The newest Doing / Next / Waiting on for `circle`, from the circle map
/// text already read from `map`. Runs off the UI thread. `None` when the read
/// failed and the caller should keep what it showed; `Some(None)` when there
/// is no log to show.
pub(super) fn read_lead_line(
    client: &crate::gui_client::GuiClient,
    map: &str,
    map_text: &str,
    circle: &str,
) -> Option<Option<LeadLine>> {
    let Some(folder) = circle_folder(map_text, circle) else {
        return Some(None);
    };
    match client.fs_read_string(&lead_log_path(map, &folder)) {
        Ok((log, _)) => Some(parse_lead_log(&log)),
        Err(e) if fs_not_found(&e) => Some(None),
        Err(_) => None,
    }
}

/// Terminals that can take the stage: not files, not popped out.
pub(super) fn stageable(p: &Pane) -> bool {
    matches!(p.body, PaneBody::Remote { .. }) && p.popped.is_none()
}

/// The pane's newest output is Claude Code's idle-compaction note.
pub(super) fn idle_compacted(snap: &GridSnapshot) -> bool {
    compacted_while_idle(&last_output_lines(
        &bottom_rows(snap, LAST_LINE_ROWS),
        LAST_LINE_ROWS,
    ))
}

/// The last `n` screen rows as text.
fn bottom_rows(snap: &GridSnapshot, n: usize) -> Vec<String> {
    let cols = snap.cols as usize;
    let rows = snap.rows as usize;
    if cols == 0 {
        return Vec::new();
    }
    (rows.saturating_sub(n)..rows)
        .filter_map(|r| snap.cells.get(r * cols..(r + 1) * cols))
        .map(|cells| cells.iter().map(|c| c.c).collect())
        .collect()
}

/// The classic | stage | chat switch, which sits after the mark.
const SWITCH_W: f32 = 190.;

/// Stage's strip height; the jump palette drops below it.
pub(super) const STRIP_H: f32 = 50.;

/// The jump box's width: "⌕ Jump" and the shortcut, or just ⌕ when the
/// window is narrow.
pub(super) fn jump_box_w(roomy: bool) -> f32 {
    match (roomy, cfg!(target_os = "macos")) {
        (false, _) => 34.,
        (true, true) => 120.,
        (true, false) => 150.,
    }
}

/// Stage and Wall's jump box: opens the jump palette just below it.
pub(super) fn render_jump_box(
    id: &'static str,
    roomy: bool,
    cx: &Context<SeanceApp>,
) -> AnyElement {
    let shortcut = if cfg!(target_os = "macos") {
        "⌘⇧J"
    } else {
        "ctrl+shift+j"
    };
    div()
        .id(id)
        .flex_none()
        .w(px(jump_box_w(roomy)))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .px(px(10.))
        .py(px(5.))
        .rounded_lg()
        .bg(SeancePalette::bg())
        .border_1()
        .border_color(SeancePalette::border())
        .text_xs()
        .text_color(SeancePalette::text_faint())
        .cursor_pointer()
        .hover(|s| s.border_color(SeancePalette::flame_dim()))
        .tooltip(tip_s(format!("jump to a circle ({shortcut})")))
        .on_click(cx.listener(|this, _, window, cx| {
            this.open_jump(window, cx);
        }))
        .child("⌕")
        .when(roomy, |d| {
            d.child(div().flex_1().min_w_0().truncate().child("Jump"))
                .child(
                    div()
                        .flex_none()
                        .px(px(5.))
                        .rounded_sm()
                        .border_1()
                        .border_color(SeancePalette::border())
                        .text_size(px(10.))
                        .child(shortcut),
                )
        })
        .into_any_element()
}

pub(super) fn glyph(text: &'static str, color: Hsla) -> AnyElement {
    div()
        .flex_none()
        .text_color(color)
        .child(text)
        .into_any_element()
}

fn host_chip(host: &str) -> gpui::Div {
    host_chip_owned(host.to_string())
}

pub(super) fn host_chip_owned(host: String) -> gpui::Div {
    div()
        .flex_none()
        .px(px(7.))
        .rounded_full()
        .border_1()
        .border_color(SeancePalette::border())
        .text_size(px(10.5))
        .text_color(SeancePalette::text_dim())
        .child(host)
}

/// The small outlined (or, `strong`, violet-filled) buttons in the strip,
/// the needs-you chips and the crew header.
pub(super) fn chip_button(label: &str, strong: bool) -> gpui::Div {
    let d = div()
        .flex_none()
        .px(px(9.))
        .py(px(2.))
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .child(label.to_string());
    if strong {
        d.bg(SeancePalette::violet())
            .text_color(SeancePalette::bg())
            .font_semibold()
            .hover(|s| s.opacity(0.85))
    } else {
        d.border_1()
            .border_color(SeancePalette::border())
            .text_color(SeancePalette::text_dim())
            .hover(|s| s.text_color(SeancePalette::text()))
    }
}
