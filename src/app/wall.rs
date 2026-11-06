//! Wall: a tile per circle in the space (or, on the All tab, in every
//! space), the selected one big with its lead live, and a Today feed of
//! what happened across circles. Focus (the button, a double click or
//! Enter) opens Stage on the selected circle, in its own space.
//!
//! Like Stage, the Wall takes the rail and grid's place, so no terminal
//! view is in the frame and the keyboard parks on the window: arrows move
//! the selection, Enter focuses, esc goes back, and typing reaches nothing.
//! The overview stream (`SetOverview`) keeps every circle's grid coming
//! while the Wall is up. The lead logs, the leads' pads and the event feed
//! are read from the daemon on background tasks when due, never from render.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, point, prelude::*, px, AnyElement, BoxShadow, Context, Entity, Hsla, SharedString, Window,
};
use gpui_component::{ActiveTheme as _, StyledExt as _};

use crate::events::{self, Event};
use crate::gui_client::fs_not_found;
use crate::remote_term_view::OverviewThumb;
use crate::theme::SeancePalette;

use super::stage::{chip_button, host_chip_owned, read_lead_line, stageable, LineCache};
use super::stage_parse::{pane_host, LeadLine};
use super::util::{now_ms, tip};
use super::wall_model::{
    aged_result, finish_excerpt, grid_cols, health_row, last_real_line, mac_waiting, merge_rows,
    move_selection, place_tiles, result_rows, rows_used, shape_feed, task_title, tile_state,
    wall_takes_keys, Cell, Dir, Dot, FeedRow, TileFacts, TileState, FEED_CAP, FEED_FOLD_BELOW,
    FEED_KINDS, RAW_CAP,
};
use super::workspaces::WorkspaceAttention;
use super::SeanceApp;

/// The feed polls the daemon this often while the Wall is up; the lead
/// logs change a few times an hour.
const FEED_EVERY: Duration = Duration::from_secs(20);
const LEAD_LOGS_EVERY: Duration = Duration::from_secs(60);
const FEED_W: f32 = 286.;
pub(super) const HEADER_H: f32 = 46.;
const GAP: f32 = 10.;
/// Rows shrink to fill the height down to this, then the grid scrolls.
const MIN_ROW_H: f32 = 140.;
/// Lines of a lead's last finish note a small tile shows.
const RESULT_LINES: usize = 3;

/// The Wall's per-window state: caches and the feed. Nothing is saved.
#[derive(Default)]
pub(super) struct WallState {
    /// The big tile's live view, kept while the same pane is up.
    thumb: Option<(String, Entity<OverviewThumb>)>,
    lines: LineCache,
    /// circle → its newest lead log entry.
    lead_lines: HashMap<String, LeadLine>,
    lead_asked: Option<Instant>,
    lead_busy: bool,
    /// The circles the last lead-log read covered.
    lead_circles: Vec<String>,
    /// lead pane → its pad's mtime and the start of its newest finish
    /// note, read on the feed's clock while the Wall is up, and only when
    /// the pad changed.
    results: HashMap<String, (Option<u64>, Vec<String>)>,
    results_asked: Option<Instant>,
    results_busy: bool,
    /// The leads the last pad read covered.
    result_leads: Vec<String>,
    /// Today's events (feed kinds only), oldest first, and the cursor.
    raw: Vec<Event>,
    seq: u64,
    day_start: u64,
    feed_asked: Option<Instant>,
    feed_busy: bool,
    /// Rows shaped from `raw`, redone only when events arrive.
    rows: Vec<FeedRow>,
    /// Rows this window saw for itself: the Mac going and coming back.
    local_rows: Vec<FeedRow>,
    /// circle → its lead was waiting on the Mac at the last look.
    mac_seen: HashMap<String, bool>,
    asleep_open: bool,
    /// The folded circles, in rail order.
    asleep: Vec<String>,
    /// Narrow windows: the feed is open over the tiles.
    feed_open: bool,
    /// The tiles as last drawn (None = the asleep tile), for arrow keys.
    order: Vec<Option<String>>,
    cells: Vec<Cell>,
}

/// One circle's tile, gathered before any element is built.
struct Tile {
    ws: String,
    label: String,
    state: TileState,
    lead: Option<String>,
    /// The lead's task, from its terminal title.
    title: Option<String>,
    doing: Option<String>,
    result: Vec<String>,
    /// The last screen line that isn't chrome, when the rest is empty.
    fallback: Option<String>,
    footer: Option<String>,
    /// The pane a needs-you tile's Open puts on stage.
    asking: Option<String>,
    /// The circle's space name, on the All tab.
    space: Option<String>,
}

impl SeanceApp {
    /// The Wall is on its All tab.
    pub(super) fn wall_all(&self) -> bool {
        self.view.mode == super::views::ViewMode::Wall && self.view.wall_all
    }

    pub(super) fn set_wall_all(&mut self, on: bool) {
        if self.view.wall_all == on {
            return;
        }
        self.view.wall_all = on;
        if self.view.persist {
            super::views::save_wall_all(on);
        }
    }

    /// Keys stop at the Wall (no pane is shown to take them) unless a
    /// field drawn over it is open: the rename field or the quicklaunch
    /// editor.
    pub(super) fn wall_owns_keys(&self) -> bool {
        wall_takes_keys(
            self.view.mode,
            self.renaming.is_some() || self.quicklaunch_editor.is_some(),
        )
    }

    /// Focus from the Wall: Stage on `ws`, with the pane waiting on you up
    /// front when there is one, else the lead.
    fn wall_focus(&mut self, ws: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Not a snoozed one: opening it would end the snooze.
        let asking = self
            .stage_waiting()
            .into_iter()
            .filter_map(|w| w.pane.filter(|p| w.circle == ws && !self.snoozed(p)))
            .next();
        self.focus_on_stage(ws, asking.as_deref(), window, cx);
    }

    /// Leaving the Wall: what it saw of the Mac goes stale once it stops
    /// looking, so the next visit starts fresh rather than log old news.
    pub(super) fn wall_leave(&mut self) {
        self.wall.mac_seen.clear();
        self.wall.results_asked = None;
    }

    /// Plain arrows move the selection, Enter focuses it. True when used.
    pub(super) fn wall_key(
        &mut self,
        ks: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let m = &ks.modifiers;
        if m.control || m.alt || m.platform || m.shift || !self.wall_owns_keys() {
            return false;
        }
        let dir = match ks.key.as_str() {
            "left" => Dir::Left,
            "right" => Dir::Right,
            "up" => Dir::Up,
            "down" => Dir::Down,
            "enter" => {
                if let Some(ws) = self.selected_workspace.clone() {
                    self.wall_focus(&ws, window, cx);
                }
                return true;
            }
            _ => return false,
        };
        let from = self
            .selected_workspace
            .as_deref()
            .and_then(|s| self.wall.order.iter().position(|t| t.as_deref() == Some(s)));
        let next = match from {
            Some(i) => move_selection(&self.wall.cells, i, dir),
            // Nothing selected here yet: the first tile.
            None => (!self.wall.order.is_empty()).then_some(0),
        };
        // The asleep tile opens and hands the selection to its first circle.
        let target = match next.and_then(|i| self.wall.order.get(i).cloned()) {
            Some(None) => {
                self.wall.asleep_open = true;
                self.wall.asleep.first().cloned()
            }
            other => other.flatten(),
        };
        if let Some(ws) = target {
            self.select_workspace(&ws, window, cx);
        }
        cx.notify();
        true
    }

    /// Fetch the lead logs and the day's events when they're due.
    fn wall_feeds(&mut self, circles: &[String], cx: &mut Context<Self>) {
        let day = events::local_day_start_ms(now_ms());
        if day != self.wall.day_start {
            // A new day: yesterday's rows go, and the cursor starts over.
            self.wall.day_start = day;
            self.wall.raw.clear();
            self.wall.rows.clear();
            self.wall.local_rows.clear();
            self.wall.seq = 0;
            self.wall.feed_asked = None;
        }
        let feed_due = !self.wall.feed_busy
            && self
                .wall
                .feed_asked
                .is_none_or(|at| at.elapsed() >= FEED_EVERY);
        if feed_due {
            self.wall.feed_busy = true;
            self.wall.feed_asked = Some(Instant::now());
            let client = Arc::clone(&self.client);
            let since = self.wall.seq;
            cx.spawn(async move |this, cx| {
                let got = cx
                    .background_executor()
                    .spawn(async move { client.recent_events(day, since, &FEED_KINDS, RAW_CAP) })
                    .await;
                let Some(this) = this.upgrade() else { return };
                this.update(cx, |app: &mut SeanceApp, cx| {
                    app.wall.feed_busy = false;
                    let Ok((evs, seq)) = got else { return };
                    if app.wall.day_start != day || app.wall.seq != since {
                        return;
                    }
                    if seq < since {
                        // The daemon's count went backwards: read the day again.
                        app.wall.raw.clear();
                        app.wall.seq = 0;
                        app.wall.feed_asked = None;
                        cx.notify();
                        return;
                    }
                    app.wall.seq = seq.max(evs.last().map_or(0, |e| e.seq));
                    if evs.is_empty() {
                        return;
                    }
                    app.wall.raw.extend(evs);
                    let over = app.wall.raw.len().saturating_sub(RAW_CAP);
                    app.wall.raw.drain(..over);
                    app.wall.rows = shape_feed(&app.wall.raw, day, FEED_CAP);
                    cx.notify();
                });
            })
            .detach();
        }

        // Each awake circle's lead pad, for its last result. Due with the
        // feed, or at once when the set of leads changes.
        let leads: Vec<String> = circles
            .iter()
            .filter(|c| !self.workspace_asleep(c))
            .filter_map(|c| self.stage_lead(c))
            .collect();
        let results_due = !self.wall.results_busy
            && (self.wall.result_leads.len() != leads.len()
                || leads.iter().any(|l| !self.wall.result_leads.contains(l))
                || self
                    .wall
                    .results_asked
                    .is_none_or(|at| at.elapsed() >= FEED_EVERY));
        if results_due {
            self.wall.results_busy = true;
            self.wall.results_asked = Some(Instant::now());
            self.wall.result_leads = leads.clone();
            let client = Arc::clone(&self.client);
            let mut had = self.wall.results.clone();
            cx.spawn(async move |this, cx| {
                let got = cx
                    .background_executor()
                    .spawn(async move {
                        leads
                            .into_iter()
                            .filter_map(|slug| {
                                let path = Self::scratch_path(&slug, "md");
                                // A failed read keeps what the card showed; only
                                // a missing pad has nothing.
                                let kept = had.remove(&slug);
                                let mtime = match client.fs_stat(&path) {
                                    Ok(Some(m)) => m,
                                    Ok(None) => return None,
                                    Err(_) => return kept.map(|o| (slug, o)),
                                };
                                // Unchanged since the last read: keep it.
                                if mtime.is_some() && kept.as_ref().is_some_and(|o| o.0 == mtime) {
                                    return kept.map(|o| (slug, o));
                                }
                                match client.fs_read_string(&path) {
                                    Ok((pad, mtime)) => {
                                        Some((slug, (mtime, finish_excerpt(&pad, RESULT_LINES))))
                                    }
                                    Err(e) if fs_not_found(&e) => None,
                                    Err(_) => kept.map(|o| (slug, o)),
                                }
                            })
                            .collect::<HashMap<_, _>>()
                    })
                    .await;
                let Some(this) = this.upgrade() else { return };
                this.update(cx, |app: &mut SeanceApp, cx| {
                    app.wall.results_busy = false;
                    if app.wall.results != got {
                        app.wall.results = got;
                        cx.notify();
                    }
                });
            })
            .detach();
        }

        let Some(map) = crate::term_config::get().seance_circle_map.clone() else {
            return;
        };
        // Due on the clock, or at once when the space shows other circles
        // (the rail reorders often; only a different set counts).
        let lead_due = !self.wall.lead_busy
            && (self.wall.lead_circles.len() != circles.len()
                || circles.iter().any(|c| !self.wall.lead_circles.contains(c))
                || self
                    .wall
                    .lead_asked
                    .is_none_or(|at| at.elapsed() >= LEAD_LOGS_EVERY));
        if lead_due && !circles.is_empty() {
            self.wall.lead_busy = true;
            self.wall.lead_asked = Some(Instant::now());
            self.wall.lead_circles = circles.to_vec();
            let client = Arc::clone(&self.client);
            let circles = circles.to_vec();
            let mut had = self.wall.lead_lines.clone();
            cx.spawn(async move |this, cx| {
                let lines = cx
                    .background_executor()
                    .spawn(async move {
                        // A failed read keeps each card's line; a missing map
                        // or log clears it.
                        let text = match client.fs_read_string(&map) {
                            Ok((text, _)) => Some(text),
                            Err(e) if fs_not_found(&e) => return HashMap::new(),
                            Err(_) => None,
                        };
                        circles
                            .into_iter()
                            .filter_map(|c| {
                                let kept = had.remove(&c);
                                let line = match &text {
                                    Some(text) => {
                                        read_lead_line(&client, &map, text, &c).unwrap_or(kept)
                                    }
                                    None => kept,
                                };
                                Some((c, line?))
                            })
                            .collect::<HashMap<_, _>>()
                    })
                    .await;
                let Some(this) = this.upgrade() else { return };
                this.update(cx, |app: &mut SeanceApp, cx| {
                    app.wall.lead_busy = false;
                    if app.wall.lead_lines != lines {
                        app.wall.lead_lines = lines;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Everything one circle's tile shows.
    fn wall_tile(
        &mut self,
        ws: &str,
        asking: Option<String>,
        all: bool,
        cx: &mut Context<Self>,
    ) -> Tile {
        let lead = self.stage_lead(ws);
        let lead_pane = lead
            .as_deref()
            .and_then(|l| self.panes.iter().find(|p| p.slug == l));
        let snap = lead_pane
            .and_then(|p| p.remote_terminal())
            .map(|t| t.read(cx).snapshot.clone());
        let lines = match (&lead, snap) {
            (Some(l), Some(snap)) => self.wall.lines.lines(l, &snap).to_vec(),
            _ => Vec::new(),
        };
        let title = lead_pane
            .filter(|p| !self.owners.get(&p.slug).is_some_and(|o| o.exited))
            .and_then(|p| task_title(&p.title(cx)?, &p.command));
        let attention = self.workspace_attention_cx(ws);
        let has_panes = self.panes.iter().any(|p| p.workspace == ws);
        let lead_exited = match &lead {
            Some(l) => self.owners.get(l).is_some_and(|o| o.exited),
            // Panes but no terminal left to lead it.
            None => has_panes,
        };
        // claude-mac's message is the bottom of the screen.
        let mac = mac_waiting(&lines[lines.len().saturating_sub(2)..]);
        let state = tile_state(TileFacts {
            asleep: self.workspace_asleep(ws),
            needs: matches!(attention, Some(WorkspaceAttention::NeedsHuman)) || asking.is_some(),
            lead_exited,
            mac_waiting: mac,
            working: matches!(attention, Some(WorkspaceAttention::Working)),
            done: matches!(attention, Some(WorkspaceAttention::Done))
                || lead.as_deref().is_some_and(|l| {
                    self.done_unseen.contains(l)
                        || self.statuses.get(l).is_some_and(|s| s.state == "done")
                }),
        });
        // No lines yet means no grid yet: nothing to compare.
        if !lines.is_empty() {
            self.note_mac(ws, mac);
        }
        let log = self.wall.lead_lines.get(ws);
        let footer = log.and_then(|l| {
            l.next
                .as_ref()
                .map(|n| format!("Next: {n}"))
                .or_else(|| l.waiting.as_ref().map(|w| format!("Waiting on: {w}")))
        });
        let doing = log.and_then(|l| l.doing.clone());
        let result = lead
            .as_deref()
            .and_then(|l| self.wall.results.get(l))
            .map(|(mtime, lines)| aged_result(lines, *mtime, now_ms()))
            .unwrap_or_default();
        let fallback = (title.is_none() && doing.is_none() && result.is_empty())
            .then(|| last_real_line(&lines))
            .flatten();
        let spaces = &self.subs_pref.spaces;
        let space = all
            .then(|| spaces.get(spaces.space_of(ws)).map(|s| s.name.clone()))
            .flatten();
        Tile {
            ws: ws.to_string(),
            label: self.workspace_label(ws),
            state,
            lead,
            title,
            doing,
            result,
            fallback,
            footer,
            asking,
            space,
        }
    }

    /// Log the Mac going away or coming back for a circle's lead, as this
    /// window sees it (the daemon logs no such event).
    fn note_mac(&mut self, ws: &str, waiting: bool) {
        let before = self.wall.mac_seen.insert(ws.to_string(), waiting);
        if before.is_none_or(|b| b == waiting) {
            return;
        }
        self.wall.local_rows.push(FeedRow {
            ts: now_ms(),
            circle: Some(ws.to_string()),
            pane: None,
            text: if waiting {
                "Mac asleep · panes waiting".into()
            } else {
                "Mac back · resuming".into()
            },
            dot: if waiting { Dot::Faint } else { Dot::Done },
            title: None,
        });
        let over = self.wall.local_rows.len().saturating_sub(FEED_CAP);
        self.wall.local_rows.drain(..over);
    }

    /// The whole Wall, in place of the rail and grid.
    pub(super) fn render_wall(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spaces = &self.subs_pref.spaces;
        let circles = if self.wall_all() {
            spaces.all_circles(&self.workspaces())
        } else {
            spaces.circles_in(&self.current_space(), &self.workspaces())
        };
        self.health_feed(cx);
        self.wall_feeds(&circles, cx);
        let size = window.viewport_size();
        let (width, height): (f32, f32) = (size.width.into(), size.height.into());
        let fold = width < FEED_FOLD_BELOW;
        let selected = self.selected_workspace.clone();

        // circle → the first pane waiting on you there, for Open.
        let mut asking: HashMap<String, String> = HashMap::new();
        for w in self.stage_waiting() {
            if let Some(p) = w.pane.filter(|p| !self.snoozed(p)) {
                asking.entry(w.circle).or_insert(p);
            }
        }
        // Space names only while the All tab is there to say why.
        let all = self.wall_all() && self.space_tabs_shown();
        let mut tiles: Vec<Tile> = Vec::with_capacity(circles.len());
        for ws in &circles {
            let ask = asking.remove(ws);
            tiles.push(self.wall_tile(ws, ask, all, cx));
        }
        let seen: Vec<&str> = circles.iter().map(String::as_str).collect();
        self.wall
            .mac_seen
            .retain(|ws, _| seen.contains(&ws.as_str()));
        self.wall
            .lines
            .retain(|s| tiles.iter().any(|t| t.lead.as_deref() == Some(s)));

        let awake = tiles
            .iter()
            .filter(|t| t.state != TileState::Asleep)
            .count();
        let needs = tiles.iter().filter(|t| t.state == TileState::Needs).count();
        let down = tiles
            .iter()
            .filter(|t| t.state == TileState::LeadExited)
            .count();

        // Sleeping circles fold into one tile; the selected one stays out.
        let (shown, asleep): (Vec<Tile>, Vec<Tile>) = tiles.into_iter().partition(|t| {
            t.state != TileState::Asleep || selected.as_deref() == Some(t.ws.as_str())
        });
        let n_asleep = asleep.len();
        self.wall.asleep = asleep.iter().map(|t| t.ws.clone()).collect();
        let mut order: Vec<Option<String>> = shown.iter().map(|t| Some(t.ws.clone())).collect();
        let mut drawn = shown;
        if n_asleep > 0 {
            order.push(None);
            if self.wall.asleep_open {
                order.extend(asleep.iter().map(|t| Some(t.ws.clone())));
                drawn.extend(asleep);
            }
        }
        let big = order
            .iter()
            .position(|t| t.is_some() && t.as_deref() == selected.as_deref());
        let grid_w = width - if fold { 0. } else { FEED_W } - 2. * GAP;
        let cols = grid_cols(grid_w);
        let cells = place_tiles(order.len(), big, cols);
        let rows = rows_used(&cells).max(1);
        let fill_rows = rows.max(3) as f32;
        let avail_h = height - HEADER_H - self.space_tabs_h() - 2. * GAP;
        let row_h = ((avail_h - GAP * (fill_rows - 1.)) / fill_rows).max(MIN_ROW_H);
        let grid_h = row_h * rows as f32 + GAP * (rows as f32 - 1.);

        let mono = cx.theme().mono_font_family.clone();
        let mut drawn_iter = drawn.into_iter();
        let mut elements = Vec::with_capacity(order.len());
        for (i, slot) in order.iter().enumerate() {
            let cell = cells[i];
            let el = match slot {
                None => self.render_asleep_tile(n_asleep, cx),
                Some(_) => {
                    let Some(tile) = drawn_iter.next() else { break };
                    if Some(i) == big {
                        self.render_big_tile(tile, cx)
                    } else {
                        self.render_small_tile(tile, row_h, mono.clone(), cx)
                    }
                }
            };
            elements.push(
                div()
                    .min_w_0()
                    .min_h_0()
                    .col_start(cell.col as i16 + 1)
                    .col_end((cell.col + cell.span) as i16 + 1)
                    .row_start(cell.row as i16 + 1)
                    .row_end((cell.row + cell.span) as i16 + 1)
                    .child(el)
                    .into_any_element(),
            );
        }
        self.wall.order = order;
        self.wall.cells = cells;

        let header = self.render_wall_header(width, awake, needs, down, fold, cx);
        let spaces = self.render_space_tabs("wall", cx);
        let grid = div()
            .id("wall-grid-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(GAP))
            .child(
                div()
                    .grid()
                    .grid_cols(cols as u16)
                    .grid_rows(rows as u16)
                    .gap(px(GAP))
                    .h(px(grid_h))
                    .children(elements),
            );
        let feed = (!fold || self.wall.feed_open).then(|| {
            let feed = self.render_wall_feed();
            if !fold {
                return feed;
            }
            // Folded and opened: the feed floats over the tiles' right edge.
            div()
                .id("wall-feed-over")
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .border_l_1()
                .border_color(SeancePalette::border())
                .occlude()
                .child(feed)
                .into_any_element()
        });

        div()
            .id("wall")
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(SeancePalette::bg())
            .child(self.render_daemon_error_bar(cx))
            .children(spaces)
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(grid)
                    .children(feed),
            )
            .into_any_element()
    }

    /// ✦ mark and AFK chip (unless the space row holds them), Wall · Focus, the layout
    /// toggle, the summary and the jump box.
    fn render_wall_header(
        &self,
        width: f32,
        awake: usize,
        needs: usize,
        down: usize,
        fold: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let seg = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex_none()
                .px(px(12.))
                .py(px(3.))
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(on, |d| {
                    d.bg(SeancePalette::flame().opacity(0.16))
                        .text_color(SeancePalette::flame())
                })
                .when(!on, |d| {
                    d.text_color(SeancePalette::text_dim())
                        .hover(|s| s.text_color(SeancePalette::text()))
                })
                .child(label)
        };
        let wall_focus = div()
            .flex_none()
            .flex()
            .p(px(2.))
            .rounded_lg()
            .border_1()
            .border_color(SeancePalette::border())
            .child(seg("wall-seg-wall", "Wall", true))
            .child(
                seg("wall-seg-focus", "Focus", false)
                    .tooltip(tip("open the selected circle on Stage (enter)"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(ws) = this.selected_workspace.clone() {
                            this.wall_focus(&ws, window, cx);
                        }
                    })),
            );
        let mut summary = div()
            .flex_none()
            .flex()
            .gap_1()
            .text_xs()
            .text_color(SeancePalette::text_dim())
            .child(format!(
                "{awake} circle{} awake",
                if awake == 1 { "" } else { "s" }
            ));
        for (n, what, color) in [
            (needs, "needs you", SeancePalette::violet()),
            (down, "down", SeancePalette::danger()),
        ] {
            if n > 0 {
                summary = summary
                    .child("·")
                    .child(div().text_color(color).child(format!("{n} {what}")));
            }
        }
        let roomy = width >= 900.;
        let rename = self.render_rename_field();
        let renaming = rename.is_some();

        div()
            .flex_none()
            .h(px(HEADER_H))
            .px(px(14.))
            .flex()
            .items_center()
            .gap(px(14.))
            .overflow_hidden()
            .bg(SeancePalette::bg_elevated())
            .border_b_1()
            .border_color(SeancePalette::border())
            // The space row holds the mark when it shows.
            .when(!self.space_tabs_shown(), |d| {
                d.child(self.render_seance_mark("wall", cx))
            })
            .children(rename)
            .when(!renaming, |d| {
                d.child(wall_focus)
                    .when(width >= 900., |d| d.child(summary))
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .w(px(170.))
                    .child(self.render_view_switch("wall", cx)),
            )
            .when(fold, |d| {
                d.child(
                    chip_button(
                        &format!("Today {}", if self.wall.feed_open { "▸" } else { "◂" }),
                        false,
                    )
                    .id("wall-feed-toggle")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.wall.feed_open = !this.wall.feed_open;
                        cx.notify();
                    })),
                )
            })
            // Left of the jump box, which the jump palette drops down from.
            .when(!self.space_tabs_shown(), |d| {
                d.children(self.render_afk_chip("wall", cx))
            })
            .child(super::stage::render_jump_box("wall-jump", roomy, cx))
            .into_any_element()
    }

    /// The selected circle: its lead live, read-only, with name, host and
    /// crew in the header and a Focus button.
    fn render_big_tile(&mut self, tile: Tile, cx: &mut Context<Self>) -> AnyElement {
        let lead = tile
            .lead
            .as_deref()
            .and_then(|l| self.panes.iter().find(|p| p.slug == l));
        let (name, host) = match lead {
            Some(p) => {
                let (n, h) = pane_host(&p.name, &p.command);
                (n.to_string(), h.map(str::to_string))
            }
            None => (String::new(), None),
        };
        let crew = self
            .panes
            .iter()
            .filter(|p| {
                p.workspace == tile.ws && stageable(p) && Some(&p.slug) != tile.lead.as_ref()
            })
            .count();
        let state = lead.map(|p| self.pane_state(p, cx));
        let (label, color) = match tile.state {
            TileState::Needs => ("● needs you".to_string(), SeancePalette::violet()),
            TileState::LeadExited => ("lead exited".to_string(), SeancePalette::danger()),
            TileState::MacAsleep => ("Mac asleep".to_string(), SeancePalette::text_faint()),
            // The crew can be working or done while the lead idles.
            TileState::Working if state.as_ref().is_none_or(|s| !s.label.starts_with('◐')) => {
                ("◐ working".to_string(), SeancePalette::flame())
            }
            TileState::Done if state.as_ref().is_none_or(|s| !s.label.starts_with('✓')) => {
                ("✓ done".to_string(), SeancePalette::success())
            }
            _ => state
                .map(|s| (s.label, s.color))
                .unwrap_or(("idle".into(), SeancePalette::text_faint())),
        };
        let terminal = lead.and_then(|p| p.remote_terminal()).cloned();
        let thumb = terminal.map(|t| {
            let slug = tile.lead.clone().unwrap_or_default();
            match &self.wall.thumb {
                Some((s, e)) if *s == slug => e.clone(),
                _ => {
                    let e = cx.new(|cx| OverviewThumb::new(t, cx));
                    self.wall.thumb = Some((slug, e.clone()));
                    e
                }
            }
        });
        let ws = tile.ws.clone();
        let ws2 = tile.ws.clone();
        let flame = SeancePalette::flame();

        div()
            .id(SharedString::from(format!("wall-big-{}", tile.ws)))
            .size_full()
            .flex()
            .flex_col()
            .rounded(px(12.))
            .overflow_hidden()
            .border_1()
            .border_color(SeancePalette::flame_dim())
            .shadow(vec![ring(flame.opacity(0.16))])
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                if ev.click_count() >= 2 {
                    this.wall_focus(&ws, window, cx);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .px(px(14.))
                    .py(px(9.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .min_w_0()
                    .bg(SeancePalette::bg_elevated())
                    .border_b_1()
                    .border_color(SeancePalette::border())
                    .text_sm()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_semibold()
                            .text_color(SeancePalette::text())
                            .child(tile.label.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(SeancePalette::text_faint())
                            .child(lead_meta((!name.is_empty()).then_some(name.as_str()), crew)),
                    )
                    .children(host.map(host_chip_owned))
                    .child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .text_xs()
                            .text_color(color)
                            .child(label),
                    )
                    .child(
                        chip_button("Focus", false)
                            .id("wall-big-focus")
                            .tooltip(tip("open on Stage (enter)"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.wall_focus(&ws2, window, cx);
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().overflow_hidden().map(|d| {
                match thumb {
                    Some(t) => d.child(t),
                    None => d
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(SeancePalette::text_dim())
                        .child("No terminal in this circle yet"),
                }
            }))
            .into_any_element()
    }

    /// Any other circle: name, status, the lead's task, Doing and last
    /// result, then Next or Waiting on.
    fn render_small_tile(
        &self,
        tile: Tile,
        row_h: f32,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let st = tile.state;
        let dim = matches!(st, TileState::MacAsleep | TileState::Asleep);
        let status: AnyElement = match st {
            TileState::Needs => div()
                .flex_none()
                .size(px(8.))
                .rounded_full()
                .bg(SeancePalette::violet())
                .into_any_element(),
            TileState::Done => status_text("✓", SeancePalette::success()),
            TileState::Working => status_text("◐", SeancePalette::flame()),
            TileState::LeadExited => status_text("lead exited", SeancePalette::danger()),
            TileState::MacAsleep => status_text("Mac asleep", SeancePalette::text_faint()),
            TileState::Asleep => status_text("☾ asleep", SeancePalette::text_faint()),
            TileState::Idle => status_text("idle", SeancePalette::text_faint()),
        };
        let text = if dim {
            SeancePalette::text_faint()
        } else {
            SeancePalette::text_dim()
        };
        let line = |l: String| div().min_w_0().truncate().child(l);
        let body: Vec<AnyElement> = if st == TileState::MacAsleep {
            ["Waiting for the Mac.", "Resumes on wake."]
                .map(|l| line(l.into()).into_any_element())
                .into()
        } else {
            let mut body = Vec::new();
            if let Some(t) = tile.title.clone() {
                body.push(
                    line(t)
                        .text_color(if dim {
                            SeancePalette::text_dim()
                        } else {
                            SeancePalette::text()
                        })
                        .into_any_element(),
                );
            }
            if let Some(d) = tile.doing.clone() {
                body.push(
                    div()
                        .min_w_0()
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(format!("Doing: {d}"))
                        .into_any_element(),
                );
            }
            // Short rows show less, so no line is cut through.
            let n = result_rows(
                row_h,
                tile.title.is_some(),
                tile.doing.is_some(),
                RESULT_LINES,
            );
            if n > 0 && !tile.result.is_empty() {
                body.push(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .text_color(SeancePalette::text_faint())
                        .children(tile.result.iter().take(n).cloned().map(line))
                        .into_any_element(),
                );
            }
            body.extend(
                tile.fallback
                    .clone()
                    .map(|l| line(l).font_family(mono).into_any_element()),
            );
            body
        };
        let border = match st {
            TileState::Needs => SeancePalette::violet(),
            TileState::LeadExited => SeancePalette::danger(),
            _ => SeancePalette::border(),
        };
        let footer: Option<AnyElement> = match st {
            TileState::Needs => {
                let (ws, pane) = (tile.ws.clone(), tile.asking.clone());
                Some(
                    chip_button("Open", true)
                        .id(SharedString::from(format!("wall-open-{}", tile.ws)))
                        .tooltip(tip("open on Stage with the pane that's asking"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.focus_on_stage(&ws, pane.as_deref(), window, cx);
                        }))
                        .into_any_element(),
                )
            }
            TileState::LeadExited => {
                let ws = tile.ws.clone();
                Some(
                    chip_button("Open", false)
                        .id(SharedString::from(format!("wall-open-{}", tile.ws)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.wall_focus(&ws, window, cx);
                        }))
                        .into_any_element(),
                )
            }
            _ => tile.footer.clone().map(|f| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child(f)
                    .into_any_element()
            }),
        };
        let ws = tile.ws.clone();
        div()
            .id(SharedString::from(format!("wall-tile-{}", tile.ws)))
            .size_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(12.))
            .border_1()
            .border_color(border)
            .cursor_pointer()
            .when(dim, |d| d.border_dashed())
            .when(!dim, |d| d.bg(SeancePalette::bg_elevated()))
            .when(st == TileState::Needs, |d| {
                d.shadow(vec![ring(SeancePalette::violet().opacity(0.18))])
            })
            .hover(|s| s.border_color(SeancePalette::flame_dim()))
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                if ev.click_count() >= 2 {
                    // The first click made this circle the big tile and the
                    // grid moved: the second lands elsewhere, so open the
                    // circle the first one picked.
                    let pick = this.selected_workspace.clone().unwrap_or(ws.clone());
                    this.wall_focus(&pick, window, cx);
                } else {
                    this.select_workspace(&ws, window, cx);
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .min_w_0()
                    .text_sm()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_semibold()
                            .text_color(if dim {
                                SeancePalette::text_dim()
                            } else {
                                SeancePalette::text()
                            })
                            .child(tile.label),
                    )
                    .children(tile.space.map(|s| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(SeancePalette::text_faint())
                            .child(s)
                    }))
                    .child(div().ml_auto().flex_none().child(status)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .text_xs()
                    .text_color(text)
                    .children(body),
            )
            .children(footer)
            .into_any_element()
    }

    /// "+N asleep": the sleeping circles, folded. A click shows or hides them.
    fn render_asleep_tile(&self, n: usize, cx: &mut Context<Self>) -> AnyElement {
        let open = self.wall.asleep_open;
        div()
            .id("wall-asleep")
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap(px(4.))
            .rounded(px(12.))
            .border_1()
            .border_dashed()
            .border_color(SeancePalette::border())
            .cursor_pointer()
            .hover(|s| s.border_color(SeancePalette::flame_dim()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.wall.asleep_open = !this.wall.asleep_open;
                cx.notify();
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(SeancePalette::text_dim())
                    .child(format!("☾ +{n} asleep")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child(if open { "hide them" } else { "show them" }),
            )
            .into_any_element()
    }

    /// TODAY: the feed, newest first, and the health report below it.
    fn render_wall_feed(&self) -> AnyElement {
        let day = self.wall.day_start;
        let health = self
            .stage
            .health
            .as_ref()
            .and_then(|h| health_row(h, &events::local_date(now_ms()), day));
        let mut extra: Vec<FeedRow> = self.wall.local_rows.clone();
        extra.extend(health);
        let rows = merge_rows(self.wall.rows.clone(), &extra, FEED_CAP);
        let empty = rows.is_empty();
        let items = rows.into_iter().map(|r| {
            let circle = match (r.title, r.circle.as_deref()) {
                (Some(t), _) => t.to_string(),
                (None, Some(c)) => self.workspace_label(c),
                (None, None) => "seance".to_string(),
            };
            let text = match (&r.pane, &r.circle) {
                // Name the pane unless it leads the circle.
                (Some(p), Some(c)) if self.stage_lead(c).as_deref() != Some(p.as_str()) => {
                    let name = self
                        .panes
                        .iter()
                        .find(|x| &x.slug == p)
                        .map_or(p.as_str(), |x| x.name.as_str());
                    format!("{name} · {}", r.text)
                }
                _ => r.text,
            };
            let time = events::fmt_time(r.ts);
            div()
                .flex()
                .gap(px(10.))
                .child(
                    div()
                        .flex_none()
                        .mt(px(5.))
                        .size(px(8.))
                        .rounded_full()
                        .bg(dot_color(r.dot)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(SeancePalette::text_faint())
                                        .child(time.get(..5).unwrap_or(&time).to_string()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(SeancePalette::text())
                                        .child(circle),
                                ),
                        )
                        .child(div().text_color(SeancePalette::text_dim()).child(text)),
                )
        });
        let usage = self.usage_items();
        let facts = match self.health_facts() {
            None if usage.is_empty() => None,
            f => Some(f.unwrap_or_default()),
        };
        let at = self.stage.health.as_ref().and_then(|h| h.at.clone());
        div()
            .w(px(FEED_W))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(14.))
            .pt(px(14.))
            .pb(px(10.))
            .bg(SeancePalette::bg_elevated())
            .border_l_1()
            .border_color(SeancePalette::border())
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child("TODAY"),
            )
            .child(
                div()
                    .id("wall-feed")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .text_sm()
                    .when(empty, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(SeancePalette::text_faint())
                                .child("Nothing yet today."),
                        )
                    })
                    .children(items),
            )
            .children(facts.map(|facts| {
                div()
                    .flex_none()
                    .pt(px(10.))
                    .border_t_1()
                    .border_color(SeancePalette::border())
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .children(usage)
                    .children(facts.into_iter().map(|(dot, text)| {
                        div()
                            .flex()
                            .gap_1()
                            .children(dot.map(|c| div().text_color(c).child("●")))
                            .child(text)
                    }))
                    .children(at.map(|at| div().child(format!("health report {at}"))))
            }))
            .into_any_element()
    }
}

/// "lead · +2 crew", or just the crew when the circle has no lead.
fn lead_meta(lead: Option<&str>, crew: usize) -> String {
    let crew = (crew > 0).then(|| format!("+{crew} crew"));
    match (lead, crew) {
        (Some(l), Some(c)) => format!("{l} · {c}"),
        (Some(l), None) => l.to_string(),
        (None, Some(c)) => c,
        (None, None) => String::new(),
    }
}

/// The soft ring around the big tile and a needs-you tile.
fn ring(color: Hsla) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(3.),
        inset: false,
    }
}

fn status_text(text: &'static str, color: Hsla) -> AnyElement {
    div()
        .flex_none()
        .text_xs()
        .text_color(color)
        .child(text)
        .into_any_element()
}

fn dot_color(dot: Dot) -> Hsla {
    match dot {
        Dot::Needs => SeancePalette::violet(),
        Dot::Done => SeancePalette::success(),
        Dot::Down => SeancePalette::danger(),
        Dot::Flame => SeancePalette::flame(),
        Dot::Faint => SeancePalette::text_faint(),
    }
}
