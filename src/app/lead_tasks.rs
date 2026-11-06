//! A circle lead's tasks card: the checklist behind its "☑ N" chip.
//!
//! The data is one daemon call (`FsOp::LeadTasks`) polled for the selected
//! circle's lead; the card is drawn from the last answer, so rendering never
//! blocks on the bridge.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    anchored, deferred, div, point, prelude::*, px, Anchor, AnyElement, Context, Pixels, Point,
    SharedString, Window,
};
use serde::Deserialize;

use crate::chat_api;
use crate::events;
use crate::theme::SeancePalette;

use super::util::{now_ms, tip};
use super::SeanceApp;

/// How often the selected circle's lead is asked for its tasks.
const POLL_EVERY: Duration = Duration::from_millis(2000);
/// A row's full text stops here, like the pad inspector's task body.
const FULL_TEXT_CAP: usize = 2500;
/// After the daemon fails to answer, ask again this rarely.
const RETRY_OFF: Duration = Duration::from_secs(600);
/// After Invoke fails to answer, the daemon's list stands in for this long.
const INVOKE_RETRY: Duration = Duration::from_secs(30);
/// Timeouts in a row before the poll goes quiet.
const TIMEOUTS_BEFORE_OFF: u8 = 3;
/// The popover card's width and its share of the window height.
const CARD_W: f32 = 420.;
const CARD_MAX_H: f32 = 0.6;

/// The open tasks card: whose, and where its chip was clicked.
pub(super) struct TasksPopover {
    pub(super) slug: String,
    pub(super) at: Point<Pixels>,
}

/// What the card draws. It comes from Invoke's `/api/tasks` (the same list
/// the phone shows) or, when Invoke cannot be reached, from the daemon's
/// `lead_tasks` answer.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(super) struct LeadTasks {
    #[serde(default)]
    tasks: Vec<TaskRow>,
    /// The daemon's busy panes; `normalized` turns them into `handed`.
    #[serde(default)]
    helpers: Vec<Helper>,
    /// In progress elsewhere: busy helper panes and the lead's background agents.
    #[serde(skip)]
    handed: Vec<Handed>,
    /// The lead's own deciding screen line while it works.
    #[serde(default)]
    evidence: Option<String>,
    /// Came from Invoke, which can resume or close a parked task; the daemon's
    /// list cannot.
    #[serde(skip)]
    invoke: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct TaskRow {
    id: String,
    status: String,
    #[serde(default)]
    created_ms: u64,
    #[serde(default)]
    finished_ms: Option<u64>,
    #[serde(default)]
    body: String,
    /// The lead's own name for the task; the user's words go under it.
    #[serde(default)]
    name: Option<String>,
    /// Sent from the phone (else from a pane).
    #[serde(default)]
    phone: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Helper {
    #[serde(default)]
    name: String,
    #[serde(default)]
    evidence: Option<String>,
}

/// One line of work in progress elsewhere, drawn as a row.
#[derive(Clone, Debug, PartialEq)]
struct Handed {
    /// Keeps a row open across refreshes; the pane's or agent's name.
    key: String,
    name: Option<String>,
    text: String,
    progress: String,
}

/// Id of the stand-in row for a pane that is busy with no task open.
const TYPED_ID: &str = "typed";

impl LeadTasks {
    /// What the chip counts: open and queued tasks. Parked ones stay out,
    /// since Seance rarely clears them.
    pub(super) fn open_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| matches!(t.status.as_str(), "open" | "queued") && t.id != TYPED_ID)
            .count()
    }

    fn with_status<'a>(&'a self, status: &'a str) -> impl Iterator<Item = &'a TaskRow> {
        self.tasks.iter().filter(move |t| t.status == status)
    }

    /// The daemon's answer, its busy panes moved to where the card looks.
    fn normalized(mut self) -> Self {
        // The daemon's rows say where they came from only in their header.
        for t in &mut self.tasks {
            t.phone |= header_end(&t.body).is_some();
        }
        self.handed = std::mem::take(&mut self.helpers)
            .into_iter()
            .map(|h| Handed {
                key: format!("helper:{}", h.name),
                name: None,
                text: h.name,
                progress: progress_of(h.evidence.as_deref()),
            })
            .collect();
        self
    }

    /// Invoke's list as the card draws it, with the phone's own rules: a
    /// helper's task is its row's words and its name the title, and a lead busy
    /// with nothing open gets a stand-in "typed into the pane" row.
    fn from_invoke(t: chat_api::Tasks) -> Self {
        let row = |t: chat_api::TaskItem, status: &str| TaskRow {
            id: t.id,
            status: if t.status.is_empty() {
                status.to_string()
            } else {
                t.status
            },
            created_ms: t.start.unwrap_or(0),
            finished_ms: t.end,
            body: t.text,
            name: t.name.filter(|n| !n.is_empty()),
            phone: t.phone,
        };
        let busy = t.busy;
        let evidence = if busy {
            progress_of(t.doing.as_deref())
        } else {
            "Waiting for its next turn".to_string()
        };
        let mut tasks: Vec<TaskRow> = Vec::new();
        for (items, status) in [(t.now, "open"), (t.waiting, "queued"), (t.parked, "parked")] {
            tasks.extend(items.into_iter().map(|i| {
                // Invoke files the row under its list; the list is the truth.
                let mut r = row(i, status);
                r.status = status.to_string();
                r
            }));
        }
        if busy && !tasks.iter().any(|r| r.status == "open") {
            tasks.insert(
                0,
                TaskRow {
                    id: TYPED_ID.into(),
                    status: "open".into(),
                    created_ms: 0,
                    finished_ms: None,
                    body: "Something typed into the pane".into(),
                    name: None,
                    phone: false,
                },
            );
        }
        tasks.extend(t.done.into_iter().map(|i| row(i, "done")));
        let mut handed: Vec<Handed> = t
            .helpers
            .into_iter()
            .map(|h| Handed {
                key: format!("helper:{}", h.name),
                name: h.task.as_ref().map(|_| h.name.clone()),
                text: h.task.unwrap_or(h.name),
                progress: progress_of(h.doing.as_deref()),
            })
            .collect();
        handed.extend(t.agents.into_iter().map(|a| Handed {
            key: format!("agent:{}", a.name),
            name: Some(a.name),
            text: a.doing.unwrap_or_default(),
            progress: format!("Background agent · running {}", a.running),
        }));
        LeadTasks {
            tasks,
            helpers: Vec::new(),
            handed,
            evidence: Some(evidence),
            invoke: true,
        }
    }
}

/// A pane's progress line minus its spinner glyph; "Working" with none.
fn progress_of(doing: Option<&str>) -> String {
    match doing.map(strip_spinner).filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None => "Working".to_string(),
    }
}

/// A task's text as the lead reads it: the phone header and the
/// "---" footer a phone task carries are cut, the rest is trimmed.
fn clean_body(body: &str) -> String {
    let body = body.split("\n---\n").next().unwrap_or(body).trim();
    let body = header_end(body).map_or(body, |end| &body[end..]);
    body.trim().to_string()
}

/// Where Invoke's header line ends, if the text opens with one: a first line
/// starting "📱 From " and ending ":" ("... on his phone (Invoke):", "...
/// in Seance's Chat on his Mac (Invoke):", "... in a browser (Invoke):").
fn header_end(body: &str) -> Option<usize> {
    let end = body.find('\n').unwrap_or(body.len());
    let line = body[..end].trim();
    (line.starts_with("📱 From ") && line.ends_with(':')).then_some(end)
}

fn first_line(body: &str) -> String {
    clean_body(body).lines().next().unwrap_or("").to_string()
}

/// Screen evidence minus its leading spinner glyph ("✻ Thinking…").
fn strip_spinner(line: &str) -> &str {
    let line = line.trim();
    let mut chars = line.chars();
    match chars.next() {
        Some(c) if !c.is_alphanumeric() && chars.as_str().starts_with(' ') => {
            chars.as_str().trim_start()
        }
        _ => line,
    }
}

/// HH:MM, with the date (MM-DD) in front of a time before today.
pub(super) fn clock(ms: u64) -> String {
    let time = &events::fmt_time(ms)[..5];
    if ms < events::local_day_start_ms(now_ms()) {
        format!("{} {time}", &events::local_date(ms)[5..])
    } else {
        time.to_string()
    }
}

fn span(ms: u64) -> String {
    let mins = ms / 60_000;
    match mins {
        0 => "under a min".into(),
        1..=59 => format!("{mins} min"),
        _ => format!("{}h {:02}m", mins / 60, mins % 60),
    }
}

impl SeanceApp {
    /// Poll for the selected circle's lead tasks.
    pub(super) fn start_lead_tasks_poll(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_EVERY).await;
            let Some(this) = this.upgrade() else { break };
            let ask = this.update(cx, |app: &mut SeanceApp, cx| {
                // The card goes when its lead, pane or layout does.
                if app.tasks_popover.is_some() && !app.tasks_popover_valid() {
                    app.close_tasks_popover(cx);
                }
                let ws = app.selected_workspace.clone()?;
                let lead = app.stage_lead(&ws)?;
                let skip_invoke = app
                    .invoke_tasks_off
                    .is_some_and(|at| at.elapsed() < INVOKE_RETRY);
                let skip_daemon = app
                    .lead_tasks_off
                    .is_some_and(|at| at.elapsed() < RETRY_OFF);
                Some((
                    lead,
                    ws,
                    Arc::clone(&app.client),
                    skip_invoke,
                    skip_daemon,
                    app.lead_tasks_gen,
                ))
            });
            let Some((lead, ws, client, skip_invoke, skip_daemon, gen)) = ask else {
                continue;
            };
            // Invoke's list first (names, handed-off work, as on the phone);
            // the daemon's only when Invoke cannot be reached.
            let got = cx
                .background_executor()
                .spawn({
                    let lead = lead.clone();
                    async move {
                        if !skip_invoke {
                            // No lead: Invoke cannot match this circle's, so its
                            // list says nothing; the daemon's does.
                            if let Ok(t) = chat_api::tasks(&ws) {
                                if t.lead.is_some() {
                                    return (Ok(LeadTasks::from_invoke(t)), true);
                                }
                            }
                        }
                        if skip_daemon {
                            return (Err(anyhow::anyhow!("daemon list paused")), false);
                        }
                        let from_daemon = client.lead_tasks(&lead).and_then(|v| {
                            serde_json::from_value::<LeadTasks>(v)
                                .map(LeadTasks::normalized)
                                .map_err(Into::into)
                        });
                        (from_daemon, false)
                    }
                })
                .await;
            let (data, via_invoke) = got;
            let data = match data {
                Ok(data) => data,
                Err(e) => {
                    // A daemon that never answers costs a 10 s wait per ask.
                    // One tunnel blip is not that: it takes a run of timeouts
                    // to stop asking for a while. (An older daemon also says so
                    // outright, see `GuiEvent::Error` in mod.rs.)
                    this.update(cx, |app: &mut SeanceApp, _| {
                        if !skip_invoke {
                            app.invoke_tasks_off = Some(Instant::now());
                        }
                        if e.to_string().contains("timed out") {
                            app.lead_tasks_timeouts = app.lead_tasks_timeouts.saturating_add(1);
                            if app.lead_tasks_timeouts >= TIMEOUTS_BEFORE_OFF {
                                app.lead_tasks_off = Some(Instant::now());
                            }
                        }
                    });
                    continue;
                }
            };
            this.update(cx, |app: &mut SeanceApp, cx| {
                // Invoke answered, or it is known to be down and the daemon did.
                // Only the daemon's own answer says the daemon is fine.
                if via_invoke {
                    app.invoke_tasks_off = None;
                } else {
                    app.lead_tasks_timeouts = 0;
                    app.lead_tasks_off = None;
                    if !skip_invoke {
                        app.invoke_tasks_off = Some(Instant::now());
                    }
                }
                // A resume or close began or ended since this was asked: the
                // card has a newer list, and this would bring a closed row back.
                if app.lead_tasks_gen == gen {
                    app.adopt_lead_tasks(lead, data, cx);
                }
            });
        })
        .detach();
    }

    /// Take a list as the lead's card, and forget the errors of rows it no
    /// longer has.
    fn adopt_lead_tasks(&mut self, lead: String, data: LeadTasks, cx: &mut Context<Self>) {
        self.tasks_error
            .retain(|id, _| data.tasks.iter().any(|t| &t.id == id));
        if self.lead_tasks.get(&lead) != Some(&data) {
            self.lead_tasks.insert(lead, data);
            cx.notify();
        }
    }

    /// Chip count and whether its popover is open. `None` for any pane that
    /// is not its circle's lead.
    pub(super) fn lead_tasks_arg(&self, pane: &super::Pane) -> Option<(usize, bool)> {
        if self.stage_lead(&pane.workspace).as_deref() != Some(pane.slug.as_str()) {
            return None;
        }
        let open = self
            .lead_tasks
            .get(&pane.slug)
            .map_or(0, LeadTasks::open_count);
        Some((open, self.tasks_popover_open(&pane.slug)))
    }

    pub(super) fn tasks_popover_open(&self, slug: &str) -> bool {
        self.tasks_popover.as_ref().is_some_and(|p| p.slug == slug)
    }

    /// Open the lead's tasks card under the chip clicked at `at`, or close it
    /// if it is already open. Focus never moves, so the terminal stays live.
    pub(super) fn toggle_tasks_popover(
        &mut self,
        slug: &str,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.tasks_popover_open(slug) {
            self.close_tasks_popover(cx);
            return;
        }
        // The lead's to-do poll follows the selected circle.
        if let Some(pane) = self.panes.iter().find(|p| p.slug == slug) {
            self.selected_workspace = Some(pane.workspace.clone());
        }
        self.tasks_popover = Some(TasksPopover {
            slug: slug.to_string(),
            at,
        });
        cx.notify();
    }

    /// Close the card. Opening never moved focus, so closing leaves it be: a
    /// composer or notes editor keeps the caret.
    pub(super) fn close_tasks_popover(&mut self, cx: &mut Context<Self>) {
        if self.tasks_popover.take().is_some() {
            cx.notify();
        }
    }

    /// The card only makes sense while its pane is still its circle's lead and
    /// the layout shows the chip (Classic or Chat).
    pub(super) fn tasks_popover_valid(&self) -> bool {
        let Some(pop) = self.tasks_popover.as_ref() else {
            return false;
        };
        matches!(
            self.view.mode,
            super::views::ViewMode::Classic | super::views::ViewMode::Chat
        ) && self
            .panes
            .iter()
            .find(|p| p.slug == pop.slug)
            .is_some_and(|p| self.stage_lead(&p.workspace).as_deref() == Some(pop.slug.as_str()))
    }

    /// The floating card: raised, with its own scroll, over a backdrop that
    /// catches the click-away (the same trick as the ✦ menu).
    pub(super) fn render_tasks_popover(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let pop = self
            .tasks_popover
            .as_ref()
            .filter(|_| self.tasks_popover_valid())?;
        let empty = LeadTasks::default();
        let data = self.lead_tasks.get(&pop.slug).unwrap_or(&empty);
        let card = div()
            .id("tasks-popover")
            .w(px(CARD_W))
            .max_h(window.viewport_size().height * CARD_MAX_H)
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(SeancePalette::border())
            .bg(SeancePalette::bg_elevated())
            .shadow_lg()
            // Clicks inside the card must not reach the backdrop.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_tasks_face(&pop.slug, data, cx)),
            );
        Some(
            deferred(
                div()
                    .id("tasks-popover-backdrop")
                    .absolute()
                    .inset_0()
                    // Swallow mouse events so a click-away does not also land
                    // in the terminal underneath.
                    .occlude()
                    .on_click(cx.listener(|this, _, _, cx| this.close_tasks_popover(cx)))
                    .child(
                        anchored()
                            .position(point(pop.at.x + px(16.), pop.at.y + px(14.)))
                            .anchor(Anchor::TopRight)
                            .snap_to_window_with_margin(px(8.))
                            .child(card),
                    ),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }

    pub(super) fn render_tasks_face(
        &self,
        slug: &str,
        data: &LeadTasks,
        cx: &Context<Self>,
    ) -> AnyElement {
        let now = now_ms();
        // The circle Invoke knows this lead by: where resume and close go.
        let ws = self
            .panes
            .iter()
            .find(|p| p.slug == slug)
            .map(|p| p.workspace.clone());
        let label = |text: &str| {
            div()
                .mt_1()
                .text_xs()
                .text_color(SeancePalette::violet())
                .child(SharedString::from(text.to_string()))
        };
        let mut face = div()
            .id(SharedString::from(format!("tasks-face-{slug}")))
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .bg(SeancePalette::bg_elevated());

        face = face.child(label("Now"));
        match data.with_status("open").next() {
            Some(t) => {
                let mut sub = Vec::new();
                if let Some(ev) = data.evidence.as_deref().map(strip_spinner) {
                    if !ev.is_empty() {
                        sub.push((ev.to_string(), SeancePalette::flame()));
                    }
                }
                if t.created_ms > 0 {
                    sub.push((
                        format!(
                            "started {} · {} so far",
                            clock(t.created_ms),
                            span(now.saturating_sub(t.created_ms))
                        ),
                        SeancePalette::text_faint(),
                    ));
                }
                face =
                    face.child(self.task_row(slug, t, "▶", SeancePalette::flame(), sub, None, cx));
            }
            None => {
                face = face.child(
                    div()
                        .text_xs()
                        .text_color(SeancePalette::text_faint())
                        .child("nothing open"),
                )
            }
        }

        if !data.handed.is_empty() {
            face = face.child(label("In progress"));
            for h in &data.handed {
                let row = TaskRow {
                    id: format!("handed-{}", h.key),
                    status: "open".into(),
                    created_ms: 0,
                    finished_ms: None,
                    body: h.text.clone(),
                    name: h.name.clone().filter(|n| !n.is_empty()),
                    phone: false,
                };
                let sub = vec![(h.progress.clone(), SeancePalette::flame())];
                face = face.child(self.task_row(
                    slug,
                    &row,
                    "▶",
                    SeancePalette::flame(),
                    sub,
                    None,
                    cx,
                ));
            }
        }

        for (title, status, glyph) in [("Up next", "queued", "☐"), ("Parked", "parked", "☐")] {
            let rows: Vec<&TaskRow> = data.with_status(status).collect();
            if rows.is_empty() {
                continue;
            }
            face = face.child(label(title));
            for t in rows {
                // A parked task was set aside when it finished its turn; with
                // no such time (Invoke gives none), just "parked".
                let sub = match (status, t.finished_ms) {
                    ("parked", Some(at)) => {
                        vec![(format!("parked {}", clock(at)), SeancePalette::text_faint())]
                    }
                    ("parked", None) => vec![("parked".to_string(), SeancePalette::text_faint())],
                    _ => vec![],
                };
                // Invoke's list can send a parked task back to the lead or
                // close it; the daemon's cannot.
                let actions = (status == "parked" && data.invoke)
                    .then(|| ws.as_deref())
                    .flatten();
                face = face.child(self.task_row(
                    slug,
                    t,
                    glyph,
                    SeancePalette::text_dim(),
                    sub,
                    actions,
                    cx,
                ));
            }
        }

        let done: Vec<&TaskRow> = data
            .tasks
            .iter()
            .filter(|t| !matches!(t.status.as_str(), "open" | "queued" | "parked"))
            .collect();
        if !done.is_empty() {
            face = face.child(label("Done lately"));
            for t in done {
                let end = t.finished_ms.unwrap_or(t.created_ms);
                let (glyph, colour, verb) = match t.status.as_str() {
                    "done" => ("☑", SeancePalette::success(), "done"),
                    "failed" => ("☒", SeancePalette::danger(), "failed"),
                    _ => ("☒", SeancePalette::danger(), "dropped"),
                };
                let sub = vec![(
                    format!(
                        "{verb} {} · took {}",
                        clock(end),
                        span(end.saturating_sub(t.created_ms))
                    ),
                    SeancePalette::text_faint(),
                )];
                face = face.child(self.task_row(slug, t, glyph, colour, sub, None, cx));
            }
        }
        face.into_any_element()
    }

    /// One checklist row: glyph, the task's name (else its first line), the user's
    /// words in faint text under a name, small lines under that. A click shows
    /// the task's full text.
    fn task_row(
        &self,
        slug: &str,
        t: &TaskRow,
        glyph: &str,
        glyph_colour: gpui::Hsla,
        sub: Vec<(String, gpui::Hsla)>,
        // The circle, when the row offers "do this next" and "close it".
        actions: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.tasks_row_open.as_deref() == Some(t.id.as_str());
        let id = t.id.clone();
        let (title, said) = row_title(t);
        let mut text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_sm()
                    .text_color(SeancePalette::text())
                    .child(title),
            )
            .children(said.map(|line| {
                div()
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child(line)
            }))
            .children(
                sub.into_iter()
                    .map(|(line, colour)| div().text_xs().text_color(colour).child(line)),
            );
        if let Some(why) = self.tasks_error.get(&t.id) {
            text = text.child(
                div()
                    .text_xs()
                    .text_color(SeancePalette::danger())
                    .child(why.clone()),
            );
        }
        if open {
            let mut full = clean_body(&t.body);
            if full.len() > FULL_TEXT_CAP {
                let mut end = FULL_TEXT_CAP;
                while !full.is_char_boundary(end) {
                    end -= 1;
                }
                full.truncate(end);
                full.push('…');
            }
            text = text.child(
                div()
                    .mt_1()
                    .p_2()
                    .rounded_md()
                    .bg(SeancePalette::bg())
                    .border_1()
                    .border_color(SeancePalette::border())
                    .text_xs()
                    .text_color(SeancePalette::text_dim())
                    .child(full),
            );
        }
        div()
            .id(SharedString::from(format!("task-row-{slug}-{}", t.id)))
            .flex()
            .gap_2()
            .px_1()
            .py_0p5()
            .rounded_sm()
            .hover(|s| s.bg(SeancePalette::surface()))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tasks_row_open = if this.tasks_row_open.as_deref() == Some(id.as_str()) {
                    None
                } else {
                    Some(id.clone())
                };
                cx.notify();
            }))
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .text_color(glyph_colour)
                    .child(glyph.to_string()),
            )
            .child(text)
            .children(actions.map(|ws| self.parked_buttons(slug, ws, &t.id, cx)))
            .into_any_element()
    }

    /// A parked row's two buttons, as on the phone: a round ↩ ("do this next":
    /// the lead gets it again as the user's message) and a small × ("close it").
    /// Both are dead while a call is running.
    fn parked_buttons(&self, slug: &str, ws: &str, id: &str, cx: &Context<Self>) -> AnyElement {
        let busy = self.tasks_busy.contains(id);
        let button = |key: &str, resume: bool| {
            let (lead, ws, id) = (slug.to_string(), ws.to_string(), id.to_string());
            let b = div()
                .id(SharedString::from(format!("task-{key}-{slug}-{id}")))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .when(busy, |d| d.opacity(0.4));
            let b = if resume {
                b.size(px(22.))
                    .rounded_full()
                    .bg(SeancePalette::flame())
                    .text_color(SeancePalette::bg())
                    .text_sm()
                    .child("↩")
                    .tooltip(tip("do this next"))
            } else {
                b.size(px(18.))
                    .rounded_full()
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child("×")
                    .tooltip(tip("close it"))
            };
            if busy {
                return b;
            }
            b.cursor_pointer()
                .hover(|s| {
                    if resume {
                        s.bg(SeancePalette::flame_dim())
                    } else {
                        s.text_color(SeancePalette::danger())
                    }
                })
                // The row itself expands on a click; the buttons are not that.
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.parked_action(lead.clone(), ws.clone(), id.clone(), resume, cx);
                    cx.stop_propagation();
                }))
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(button("resume", true))
            .child(button("dismiss", false))
            .into_any_element()
    }

    /// "Do this next" (`resume`) or "close it" on a parked task, through
    /// Invoke. The card refreshes after; a refusal shows under the row.
    fn parked_action(
        &mut self,
        lead: String,
        ws: String,
        id: String,
        resume: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.tasks_busy.insert(id.clone()) {
            return;
        }
        self.tasks_error.remove(&id);
        self.lead_tasks_gen += 1;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (call_ws, call_id) = (ws.clone(), id.clone());
            let done = cx
                .background_executor()
                .spawn(async move {
                    if resume {
                        chat_api::resume(&call_ws, &call_id)
                    } else {
                        chat_api::dismiss(&call_ws, &call_id)
                    }
                })
                .await;
            // Read the list again now, rather than at the next poll.
            let fresh = match done {
                Ok(()) => {
                    let ask = ws.clone();
                    cx.background_executor()
                        .spawn(async move { chat_api::tasks(&ask) })
                        .await
                        .ok()
                }
                Err(_) => None,
            };
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                app.tasks_busy.remove(&id);
                // Polls asked while the call ran are older than what it found.
                app.lead_tasks_gen += 1;
                match done {
                    Ok(()) => {
                        if let Some(t) = fresh.filter(|t| t.lead.is_some()) {
                            app.adopt_lead_tasks(lead, LeadTasks::from_invoke(t), cx);
                        }
                    }
                    Err(e) => {
                        app.tasks_error.insert(id, e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// A row's title and, under a lead-given name, the user's words ("You: " from the
/// phone, "Asked: " from a pane).
fn row_title(t: &TaskRow) -> (String, Option<String>) {
    let words = first_line(&t.body);
    match t.name.as_deref().filter(|n| !n.is_empty()) {
        Some(name) if !words.is_empty() => {
            let who = if t.phone { "You" } else { "Asked" };
            (name.to_string(), Some(format!("{who}: {words}")))
        }
        Some(name) => (name.to_string(), None),
        None => (words, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_task_loses_header_and_footer() {
        for header in [
            "📱 From Ann's phone (Invoke):",
            "📱 From Ann on his phone (Invoke):",
            "📱 From Ann in Seance's Chat on his Mac (Invoke):",
            "📱 From Ann in a browser (Invoke):",
        ] {
            let body =
                format!("{header}\nship the icon\nmore detail\n---\nreply with seance ctl finish");
            assert_eq!(clean_body(&body), "ship the icon\nmore detail", "{header}");
            assert_eq!(first_line(&body), "ship the icon", "{header}");
        }
        // Only a header line is cut: words that merely start that way stay.
        assert_eq!(
            clean_body("📱 From Ann, a thought"),
            "📱 From Ann, a thought"
        );
    }

    #[test]
    fn the_daemons_rows_with_a_header_count_as_from_the_phone() {
        let raw: LeadTasks = serde_json::from_str(
            r#"{"tasks":[
            {"id":"t1","status":"done","body":"📱 From Ann in a browser (Invoke):\nhi"},
            {"id":"t2","status":"done","body":"from a pane"}]}"#,
        )
        .unwrap();
        let d = raw.normalized();
        assert!(d.tasks[0].phone);
        assert!(!d.tasks[1].phone);
    }

    #[test]
    fn plain_task_is_kept() {
        assert_eq!(first_line("  build the thing\nsecond"), "build the thing");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn spinner_glyph_is_stripped_but_words_are_not() {
        assert_eq!(strip_spinner("✻ Thinking… (12s)"), "Thinking… (12s)");
        assert_eq!(strip_spinner("Running tests"), "Running tests");
        assert_eq!(strip_spinner("  · Reading"), "Reading");
    }

    #[test]
    fn spans_read_in_minutes_then_hours() {
        assert_eq!(span(20_000), "under a min");
        assert_eq!(span(5 * 60_000), "5 min");
        assert_eq!(span(135 * 60_000), "2h 15m");
    }

    #[test]
    fn chip_counts_open_and_queued_not_parked() {
        let row = |status: &str| TaskRow {
            id: status.into(),
            status: status.into(),
            created_ms: 0,
            finished_ms: None,
            body: String::new(),
            name: None,
            phone: false,
        };
        let data = LeadTasks {
            tasks: vec![row("open"), row("queued"), row("parked"), row("done")],
            ..Default::default()
        };
        assert_eq!(data.open_count(), 2);
    }

    fn item(id: &str, text: &str, name: Option<&str>, phone: bool) -> chat_api::TaskItem {
        chat_api::TaskItem {
            id: id.into(),
            text: text.into(),
            name: name.map(Into::into),
            phone,
            start: Some(1_000),
            ..Default::default()
        }
    }

    #[test]
    fn invoke_lists_become_rows_by_list_with_names_kept() {
        let t = chat_api::Tasks {
            lead: Some("L".into()),
            busy: true,
            doing: Some("✻ Reading the code (4s)".into()),
            now: vec![item("task-1", "fix it", Some("Fix the thing"), true)],
            waiting: vec![item("task-2", "later", None, false)],
            parked: vec![item("task-3", "old", None, false)],
            done: vec![chat_api::TaskItem {
                status: "orphaned".into(),
                ..item("task-4", "gone", None, true)
            }],
            ..Default::default()
        };
        let d = LeadTasks::from_invoke(t);
        let by: Vec<(&str, &str)> = d
            .tasks
            .iter()
            .map(|r| (r.id.as_str(), r.status.as_str()))
            .collect();
        assert_eq!(
            by,
            [
                ("task-1", "open"),
                ("task-2", "queued"),
                ("task-3", "parked"),
                ("task-4", "orphaned"),
            ]
        );
        assert_eq!(d.open_count(), 2);
        assert_eq!(d.evidence.as_deref(), Some("Reading the code (4s)"));
        let (title, said) = row_title(&d.tasks[0]);
        assert_eq!(title, "Fix the thing");
        assert_eq!(said.as_deref(), Some("You: fix it"));
    }

    #[test]
    fn a_busy_lead_with_nothing_open_gets_a_stand_in_row_that_is_not_counted() {
        let d = LeadTasks::from_invoke(chat_api::Tasks {
            busy: true,
            ..Default::default()
        });
        assert_eq!(d.tasks[0].body, "Something typed into the pane");
        assert_eq!(d.open_count(), 0);
        let idle = LeadTasks::from_invoke(chat_api::Tasks::default());
        assert!(idle.tasks.is_empty());
        assert_eq!(idle.evidence.as_deref(), Some("Waiting for its next turn"));
    }

    #[test]
    fn handed_off_work_follows_the_phone_rules() {
        let d = LeadTasks::from_invoke(chat_api::Tasks {
            helpers: vec![
                chat_api::HelperItem {
                    name: "Builder".into(),
                    doing: Some("✽ Building (6s)".into()),
                    task: Some("build it".into()),
                },
                chat_api::HelperItem {
                    name: "Scout".into(),
                    ..Default::default()
                },
            ],
            agents: vec![chat_api::AgentItem {
                name: "review".into(),
                doing: Some("reading the diff".into()),
                running: "5 min".into(),
            }],
            ..Default::default()
        });
        assert_eq!(d.handed.len(), 3);
        assert_eq!(d.handed[0].name.as_deref(), Some("Builder"));
        assert_eq!(d.handed[0].text, "build it");
        assert_eq!(d.handed[0].progress, "Building (6s)");
        assert_eq!(d.handed[1].name, None);
        assert_eq!(d.handed[1].text, "Scout");
        assert_eq!(d.handed[1].progress, "Working");
        assert_eq!(d.handed[2].progress, "Background agent · running 5 min");
    }

    #[test]
    fn the_daemons_helpers_move_to_handed_off() {
        let raw: LeadTasks = serde_json::from_str(
            r#"{"tasks":[],"helpers":[{"name":"Builder","evidence":"✽ Building"}],"evidence":null}"#,
        )
        .unwrap();
        let d = raw.normalized();
        assert!(d.helpers.is_empty());
        assert_eq!(d.handed[0].text, "Builder");
        assert_eq!(d.handed[0].progress, "Building");
    }

    #[test]
    fn rows_without_a_name_use_their_first_line() {
        let row = TaskRow {
            id: "t".into(),
            status: "done".into(),
            created_ms: 0,
            finished_ms: None,
            body: "ok can you do it plz.\nmore".into(),
            name: None,
            phone: true,
        };
        assert_eq!(row_title(&row), ("ok can you do it plz.".to_string(), None));
    }

    #[test]
    fn only_invoke_lists_offer_resume_and_close() {
        assert!(LeadTasks::from_invoke(chat_api::Tasks::default()).invoke);
        let daemon: LeadTasks = serde_json::from_str(r#"{"tasks":[],"helpers":[]}"#).unwrap();
        assert!(!daemon.normalized().invoke);
    }
}
