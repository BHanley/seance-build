//! The Chat layout: circles on the left, the selected circle's thread on
//! the right, drawn from Invoke's own history (`chat_api`) so the phone and
//! this window share one conversation. No terminal renders here; "show
//! terminal" goes back to Classic on the circle's lead.
//!
//! A background loop refreshes the circle list and the open thread, then
//! long-polls Invoke's change counter. Rendering reads the last answers.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    div, img, prelude::*, px, relative, AnyElement, Context, Entity, Focusable as _, ScrollHandle,
    SharedString, WeakEntity, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::{StyledExt as _, WindowExt as _};
use regex::Regex;

use crate::chat_api::{
    self, Attachment, CircleRow, Circles, Hit, Msg, PaneRow, Progress, Thread, Usage, UsageWindow,
};
use crate::events;
use crate::theme::SeancePalette;

use super::lead_tasks::clock;
use super::util::{now_ms, tip};
use super::views::ViewMode;
use super::SeanceApp;

/// The long-poll waits this long for Invoke to report a change.
const POLL_SECS: u64 = 25;
/// After a failed round, wait before trying again.
const RETRY: Duration = Duration::from_secs(3);
/// While another layout is up the loop only checks back this often.
const IDLE: Duration = Duration::from_secs(1);
const LIST_W: f32 = 300.;
/// A list row's crew against the header's, as the web Chat scales it.
const LIST_CREW: f32 = 0.62;
/// Invoke's alerts circle is read-only: nothing to say, no lead to open.
const ALERTS_WS: &str = "alerts";
/// A refresh reads this many of the newest messages again, to catch status changes.
const REFRESH_WINDOW: usize = 20;
/// A failed read report waits this long before the next try.
const READ_RETRY: Duration = Duration::from_secs(30);
/// A circle with a message newer than this is Active, as on the phone.
const ACTIVE_MS: u64 = 2 * 3600 * 1000;
/// The pane list for @-mentions is asked again after this.
const PANES_TTL: Duration = Duration::from_secs(30);
/// Within this many points of the bottom, the thread counts as at the bottom.
const NEAR_BOTTOM: f32 = 80.;

/// Follow the bottom as the web app does. `now` is the scroll offset (zero or
/// negative) and the scroll range as the last frame laid them out; `seen` is
/// the same a frame earlier. A change of offset with the range unchanged is
/// the user scrolling (growth moves the range, not the offset). Returns whether
/// the thread is pinned and whether to scroll it down: only while pinned, and
/// never while `held` (a search result is being shown).
fn follow_step(
    pinned: bool,
    held: bool,
    seen: Option<(f32, f32)>,
    now: (f32, f32),
) -> (bool, bool) {
    let from_bottom = now.1 + now.0;
    let by_hand =
        seen.is_some_and(|(off, max)| (off - now.0).abs() > 0.5 && (max - now.1).abs() < 0.5);
    let grew = seen.is_none_or(|(_, max)| (max - now.1).abs() >= 0.5);
    let mut pinned = if by_hand {
        from_bottom <= NEAR_BOTTOM
    } else {
        pinned
    };
    // A resize or shrink can clamp him onto the bottom without a scroll.
    if !by_hand && from_bottom <= 0.5 {
        pinned = true;
    }
    // Only a change of range scrolls down; a small scroll by hand near the
    // bottom stays where he put it.
    (
        pinned,
        pinned && !held && !by_hand && grew && from_bottom > 0.5,
    )
}

/// Search runs this long after the last keystroke.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// Fewest characters a search is run for (Invoke answers nothing for one).
const SEARCH_MIN: usize = 2;
/// What a click on the header's creature set going: its circle, the move it
/// plays (none for a party) from `at`, and the line in its bubble.
struct Egg {
    ws: String,
    mv: Option<usize>,
    at: u64,
    line: &'static str,
}

/// What the quick-reply chips above an empty composer send.
const QUICK_REPLIES: [&str; 3] = ["Keep going", "Status?", "Ship it"];

#[derive(Clone, Debug, PartialEq)]
enum VideoState {
    Downloading,
    Failed(String),
}

/// Pictures a lead sends as a note can also be renders. Those are not pictures:
/// no download until someone asks, and then with time to spare.
fn is_video(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp4" | "webm" | "mov" | "m4v"
    )
}

/// A video's download may take this long.
const VIDEO_SECS: u64 = 900;

#[derive(Default)]
pub(super) struct ChatState {
    circles: Vec<CircleRow>,
    selected: Option<String>,
    threads: HashMap<String, Thread>,
    progress: HashMap<String, Progress>,
    /// Invoke's change counter as of the last round.
    v: u64,
    input: Option<Entity<InputState>>,
    scroll: ScrollHandle,
    /// Frames left to pin the thread to its newest message.
    stick: u8,
    /// the user scrolled up to read: new messages and growth leave the thread
    /// where it is until he is back near the bottom or opens a circle.
    unpinned: bool,
    /// Scroll offset and range as the last frame saw them, to tell his
    /// scrolling from the content growing.
    seen: Option<(f32, f32)>,
    /// Highest id Invoke confirmed read, per circle.
    read_sent: HashMap<String, i64>,
    /// Circles with a read report in flight.
    read_busy: HashSet<String>,
    /// A failed read report is not retried before this.
    read_retry_at: HashMap<String, Instant>,
    /// Whether this window was the active one at the last frame.
    window_active: bool,
    /// The header crew's run of moves, while it shows.
    crew_dance: Option<super::sprite::CrewDance>,
    /// Each list row's crew dance by circle, apart from the header's.
    list_dances: HashMap<String, super::sprite::CrewDance>,
    /// Rows whose crew dances this frame: those scrolled into view.
    list_dancing: HashSet<String>,
    list_scroll: ScrollHandle,
    /// Counts down from zero: ids for bubbles shown before Invoke has them.
    next_local: i64,
    /// Text of a send that failed, to put back in the composer.
    restore: Option<String>,
    /// Focus the composer at the next frame (switching into Chat).
    pub(super) focus_input: bool,
    /// Image name to its cached file, once fetched.
    images: HashMap<String, (PathBuf, u32, u32)>,
    /// Videos being fetched on a click, or that failed to be.
    videos: HashMap<String, VideoState>,
    /// Choices tapped here, by the message they answer, until Invoke's own
    /// copy of the thread says so too.
    tapped: HashMap<i64, String>,
    image_asked: HashSet<String>,
    error: Option<String>,
    /// Claude plan usage from the last thread answer; none hides the strip.
    usage: Option<Usage>,
    search: Option<Entity<InputState>>,
    /// What the search found; none while the box is empty (the circles show).
    hits: Option<Vec<Hit>>,
    /// Counts keystrokes in the search box, so only the last one searches.
    search_seq: u64,
    /// Under the search box: "No results", or why the search failed.
    search_note: Option<String>,
    /// A message to scroll to once its thread is showing.
    jump: Option<i64>,
    /// A circle scrolled to a search result: new messages do not pin it to
    /// the bottom until the user sends something or opens another circle.
    no_pin: Option<String>,
    /// A circle marked unread while open: not read again until selected.
    hold_read: Option<String>,
    /// Each circle's panes for @-mentions, and when they were asked for.
    panes: HashMap<String, Vec<PaneRow>>,
    panes_at: HashMap<String, Instant>,
    /// Recent clicks on the header's creature, and what the last one did.
    pokes: super::sprite::Pokes,
    egg: Option<Egg>,
    /// The five-click party, while it runs.
    party: Option<super::sprite::Party>,
    /// Where the header's creature was last painted: a click there during a
    /// party is one more poke, ignored, not the click that ends it.
    creature_at: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// When Compact was last clicked, for its cooldown.
    compact_at: Option<(String, Instant)>,
    /// Why the last action the user took was refused; a click clears it.
    notice: Option<String>,
}

impl ChatState {
    /// Newest message Invoke has given us for `ws`: a refresh asks only for
    /// what comes after it.
    fn last_id(&self, ws: &str) -> Option<i64> {
        self.threads
            .get(ws)?
            .messages
            .iter()
            .map(|m| m.id)
            .filter(|id| *id > 0)
            .max()
    }

    /// Where a refresh of `ws` starts: the last `REFRESH_WINDOW` messages again,
    /// not only the new ones, because a note's status moves on after it is sent
    /// (sending, got, answered) and a delta of new messages alone would never
    /// say so. A short thread is read whole.
    fn refresh_after(&self, ws: &str) -> Option<i64> {
        let mut ids: Vec<i64> = self
            .threads
            .get(ws)?
            .messages
            .iter()
            .map(|m| m.id)
            .filter(|id| *id > 0)
            .collect();
        ids.sort_unstable();
        ids.len().checked_sub(REFRESH_WINDOW + 1).map(|at| ids[at])
    }

    /// Coming back to Chat: open at the newest message again.
    pub(super) fn arrive(&mut self) {
        self.stick = 3;
        self.unpinned = false;
        self.no_pin = None;
        self.jump = None;
    }

    pub(super) fn any_working(&self) -> bool {
        self.circles.iter().any(|c| c.working)
    }
}

/// One round's answers; a failed call leaves its part as it was.
struct Round {
    circles: Option<Circles>,
    /// The thread, and whether it holds only the messages past an id.
    thread: Option<(String, Thread, bool)>,
    progress: Option<(String, Progress)>,
    error: Option<String>,
}

fn fetch_round(ws: Option<String>, after: Option<i64>) -> Round {
    let mut round = Round {
        circles: None,
        thread: None,
        progress: None,
        error: None,
    };
    match chat_api::circles() {
        Ok(c) => round.circles = Some(c),
        Err(e) => round.error = Some(e.to_string()),
    }
    if let (Some(ws), None) = (ws, &round.error) {
        round.thread = chat_api::thread(&ws, after)
            .ok()
            .map(|t| (ws.clone(), t, after.is_some()));
        round.progress = chat_api::progress(&ws).ok().map(|p| (ws, p));
    }
    round
}

fn url_re() -> &'static Regex {
    static URL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    URL.get_or_init(|| Regex::new(r"https?://[^\s<>]+").unwrap())
}

/// A link's end: trailing punctuation is the sentence's, not the URL's.
fn trim_link(url: &str) -> &str {
    url.trim_end_matches(['.', ',', ')', ']', ';', ':', '!', '?'])
}

/// A message as markdown that reads back as the same text: every ASCII
/// punctuation mark escaped, every line its own paragraph (the renderer drops
/// markdown's hard breaks, and the view draws paragraphs with no gap), leading spaces kept (as
/// non-breaking ones, so indentation survives and four of them are not a code
/// block), URLs turned into links. The selectable text view draws it, so the
/// bubble keeps its look and gains selection, copy and clickable links.
fn to_markdown(text: &str) -> String {
    const NBSP: char = '\u{a0}';
    let escape = |s: &str| -> String {
        s.chars()
            .flat_map(|c| {
                let esc = c.is_ascii_punctuation().then_some('\\');
                esc.into_iter().chain(std::iter::once(c))
            })
            .collect()
    };
    let mut lines: Vec<&str> = text.lines().collect();
    // A message does not end in blank space.
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|line| {
            if line.trim().is_empty() {
                // A blank line still takes its height.
                return NBSP.to_string();
            }
            let body = line.trim_start_matches([' ', '\t']);
            let indent: String = line[..line.len() - body.len()]
                .chars()
                .flat_map(|c| std::iter::repeat_n(NBSP, if c == '\t' { 4 } else { 1 }))
                .collect();
            let (mut out, mut at) = (indent, 0);
            for m in url_re().find_iter(body) {
                let link = trim_link(m.as_str());
                out.push_str(&escape(&body[at..m.start()]));
                out.push_str(&format!("[{}](<{link}>)", escape(link)));
                at = m.start() + link.len();
            }
            out.push_str(&escape(&body[at..]));
            out
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The file paths in `text` (whitespace-separated words starting `/` or
/// `~/`, wrapping quotes and trailing punctuation left out), with where each
/// sits in it.
fn path_spans(text: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut spans = Vec::new();
    let mut at = 0;
    for word in text.split_inclusive(char::is_whitespace) {
        let start = at;
        at += word.len();
        let lead = word.len()
            - word
                .trim_start_matches(['(', '[', '<', '"', '\'', '`'])
                .len();
        let token = word
            .trim_start_matches(['(', '[', '<', '"', '\'', '`'])
            .trim_end_matches(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        ')' | ']' | '>' | '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '!' | '?'
                    )
            });
        if (token.starts_with('/') || token.starts_with("~/")) && token.len() > 1 {
            let from = start + lead;
            spans.push((from..from + token.len(), token));
        }
    }
    spans
}

/// The path a double-clicked `word` belongs to, if it can only be that one:
/// the word has a letter or digit in it (gpui picks a lone "/" or "." on
/// punctuation), every place it appears in the message is inside a path, and
/// those places name one path. "png" in "I put the png in ~/out/a.png" is a
/// word of the sentence, not a click on the file.
fn path_of_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    if !word.chars().any(char::is_alphanumeric) {
        return None;
    }
    let spans = path_spans(text);
    let mut found: Option<&str> = None;
    for (at, _) in text.match_indices(word) {
        let end = at + word.len();
        let (_, path) = spans.iter().find(|(r, _)| r.start <= at && end <= r.end)?;
        match found {
            Some(p) if p != *path => return None,
            _ => found = Some(path),
        }
    }
    found
}

/// Double-click in a bubble: the text view has just selected a word. If it sits
/// in a file path of the message that exists here, open it the way the
/// terminal does: images, videos and pages in their app, anything else shown
/// in Finder.
fn open_path_under_double_click(text: &str, window: &mut Window, cx: &mut gpui::App) {
    let word = window.selected_text(cx);
    let Some(printed) = path_of_word(text, word.trim()) else {
        return;
    };
    let Some(path) = crate::remote_term_view::existing_path(printed) else {
        return;
    };
    if path.is_file() && crate::remote_term_view::opens_in_app(&path) {
        crate::sysopen::open_detached(&path.to_string_lossy());
    } else {
        crate::sysopen::reveal_detached(&path);
    }
}

/// One-line preview of a message for the circle list.
fn preview(m: &Msg) -> String {
    let text = m.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if text.is_empty() && !m.image_names().is_empty() {
        "📷 photo".to_string()
    } else {
        text
    };
    if m.from == "me" {
        format!("You: {text}")
    } else {
        text
    }
}

/// Circles Invoke's phone list shows under Active: not alerts, with unread
/// messages or a last message under two hours old, newest first.
fn active_circles(circles: &[CircleRow], now: u64) -> Vec<CircleRow> {
    let mut active: Vec<CircleRow> = circles
        .iter()
        .filter(|c| {
            c.ws != ALERTS_WS
                && (c.unread > 0
                    || c.last
                        .as_ref()
                        .is_some_and(|m| now.saturating_sub(m.ts) < ACTIVE_MS))
        })
        .cloned()
        .collect();
    active.sort_by_key(|c| std::cmp::Reverse(c.last.as_ref().map_or(0, |m| m.ts)));
    active
}

/// 12-hour clock, as the phone shows it: "12:50 PM", or with a weekday and
/// no minutes for the weekly reset: "Thu 2 AM".
fn reset_label(ts_ms: u64, with_day: bool) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let (day, hour, minute) = events::local_clock(ts_ms);
    let h12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    let ampm = if hour < 12 { "AM" } else { "PM" };
    if with_day {
        format!("{} {h12} {ampm}", DAYS[day as usize % 7])
    } else {
        format!("{h12}:{minute:02} {ampm}")
    }
}

/// Context use from which the number is yellow, and from which it is orange
/// and Compact is offered.
const CTX_YELLOW: u32 = 30;
const CTX_ORANGE: u32 = 50;
/// A Compact click is not taken again for this long.
const COMPACT_COOLDOWN: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Heat {
    Calm,
    Yellow,
    Orange,
}

impl Heat {
    fn color(self) -> gpui::Hsla {
        match self {
            Self::Calm => SeancePalette::text_faint(),
            Self::Yellow => gpui::rgb(0xe8c34a).into(),
            Self::Orange => gpui::rgb(0xff9f43).into(),
        }
    }
}

fn ctx_heat(percent: u32) -> Heat {
    match percent {
        p if p >= CTX_ORANGE => Heat::Orange,
        p if p >= CTX_YELLOW => Heat::Yellow,
        _ => Heat::Calm,
    }
}

/// The thin usage line under the thread header, like the phone's: "5h 18% ·
/// ⟳ 12:50 PM   7d 35% · ⟳ Thu 2 AM   ctx 28%". Numbers go red from 80%, the
/// last (ctx, the lead's own context and not the plan's) turns yellow from 30%
/// and orange from 50%, never red; from 50% `compact` (a button) follows it.
/// None when nothing is known.
fn usage_strip(
    u: Option<&Usage>,
    context: Option<u32>,
    compact: Option<AnyElement>,
) -> Option<AnyElement> {
    let part = |label: &str, used: f64, color: gpui::Hsla, resets: Option<(u64, bool)>| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(label.to_string())
            .child(
                div()
                    .font_semibold()
                    .text_color(color)
                    .child(format!("{}%", used.round() as i64)),
            )
            .children(resets.map(|(at, with_day)| format!("· ⟳ {}", reset_label(at, with_day))))
    };
    let window = |label: &str, w: &UsageWindow, with_day: bool| {
        let color = if w.used >= 80. {
            SeancePalette::danger()
        } else {
            SeancePalette::text_faint()
        };
        part(label, w.used, color, w.resets.map(|at| (at, with_day)))
    };
    let parts: Vec<_> = [
        u.and_then(|u| u.five_hour.as_ref())
            .map(|w| window("5h", w, false)),
        u.and_then(|u| u.seven_day.as_ref())
            .map(|w| window("7d", w, true)),
        context.map(|c| {
            part("ctx", f64::from(c), ctx_heat(c).color(), None)
                .when(c >= CTX_ORANGE, |d| d.children(compact))
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return None;
    }
    Some(
        div()
            .flex_none()
            .py_1()
            .flex()
            .justify_center()
            .gap_4()
            .text_xs()
            .text_color(SeancePalette::text_faint())
            .children(parts)
            .into_any_element(),
    )
}

/// The bubble's text keeps the bubble's own look: no gap between lines and the
/// colors and size of the bubble around it.
fn bubble_text_style() -> TextViewStyle {
    TextViewStyle {
        paragraph_gap: gpui::rems(0.),
        is_dark: !crate::theme::is_light(),
        ..Default::default()
    }
}

/// A picture in a bubble is at most this wide and this tall, at its own shape;
/// a box this tall stands in until the file has arrived.
const IMG_MAX_W: f32 = 320.;
const IMG_MAX_H: f32 = 420.;
const IMG_LOADING_H: f32 = 240.;

/// The box a `w` by `h` picture takes in a bubble: its own shape, shrunk to fit
/// the limits, never enlarged. An unknown size gets the loading box.
fn fit_image(w: u32, h: u32) -> (f32, f32) {
    if w == 0 || h == 0 {
        return (IMG_MAX_W, IMG_LOADING_H);
    }
    let (w, h) = (w as f32, h as f32);
    let scale = (IMG_MAX_W / w).min(IMG_MAX_H / h).min(1.);
    ((w * scale).round().max(1.), (h * scale).round().max(1.))
}

/// The tick for a message, as (glyph, accent color, tooltip): the user's notes by
/// status, a lead's messages once their id is at or under what the user has read.
fn tick_for(m: &Msg, read: i64) -> Option<(&'static str, bool, &'static str)> {
    match (m.from.as_str(), m.status.as_str()) {
        ("me", "sending") => Some(("✓", false, "Waiting for the lead")),
        ("me", "got") => Some(("✓✓", true, "The lead has seen it")),
        ("me", "answered") => Some(("✓✓", true, "Answered")),
        ("lead", _) if m.id > 0 && m.id <= read => Some(("✓✓", true, "Seen")),
        _ => None,
    }
}

/// One child of the circle list.
enum ListItem {
    Label(String),
    Circle(CircleRow),
}

/// A list heading: a space's name, or Active.
fn list_label(text: String) -> gpui::Div {
    div()
        .px_3()
        .pt_2()
        .pb_1()
        .text_xs()
        .text_color(SeancePalette::text_faint())
        .child(text)
}

/// The reminder choices, as the menu words them and as Invoke's `label` does.
const REMIND_CHOICES: [(&str, &str); 4] = [
    ("In 1 hour", "in 1 hour"),
    ("In 3 hours", "in 3 hours"),
    ("Tomorrow at 9 AM", "tomorrow at 9 AM"),
    ("Monday at 9 AM", "Monday at 9 AM"),
];

/// When each of `REMIND_CHOICES` falls (unix ms), by this machine's clock.
/// The 9 AM ones start from a point at midday so a clock change cannot move
/// them to the wrong day.
fn remind_times(now: u64) -> [u64; 4] {
    const HOUR: u64 = 3600 * 1000;
    let today = events::local_day_start_ms(now);
    // Midnight plus 9 hours, moved back onto 9:00 on a 23 or 25 hour day.
    let nine = |probe: u64| {
        let at = events::local_day_start_ms(probe) + 9 * HOUR;
        let hour = u64::from(events::local_clock(at).1);
        (at + 9 * HOUR).saturating_sub(hour * HOUR)
    };
    let monday = (1..=7)
        .map(|d| today + d * 24 * HOUR + 12 * HOUR)
        .find(|probe| events::local_clock(*probe).0 == 1)
        .map_or(0, nine);
    [now + HOUR, now + 3 * HOUR, nine(today + 36 * HOUR), monday]
}

/// A success toast on the window the menu was used in, as the other menus
/// show one.
fn toast(win: gpui::AnyWindowHandle, cx: &mut gpui::App, msg: String) {
    let _ = win.update(cx, |_, window, cx| {
        window.push_notification(gpui_component::notification::Notification::success(msg), cx)
    });
}

/// The words to search for, or why not: nothing typed (no message) and too
/// short (a hint to show).
fn search_query(raw: &str) -> Result<&str, Option<&'static str>> {
    let q = raw.trim();
    match q.chars().count() {
        0 => Err(None),
        n if n < SEARCH_MIN => Err(Some("Type at least 2 characters")),
        _ => Ok(q),
    }
}

/// A spec card's body: the text without its first line, which is the title.
fn spec_body(text: &str) -> &str {
    text.split_once('\n')
        .map_or("", |(_, rest)| rest.trim_start_matches('\n'))
}

/// Change on a spec card sends nothing: it hands the keyboard to the composer.
fn changes_spec(m: &Msg, choice: &str) -> bool {
    m.spec.is_some() && choice.eq_ignore_ascii_case("change")
}

/// A note that is only its document: the server words it "📎 <title>".
fn attachment_only(m: &Msg) -> bool {
    m.attachment
        .as_ref()
        .is_some_and(|a| m.text == format!("📎 {}", a.title))
}

/// A document on a note: its title, as a link that opens it.
fn attachment_link(id: i64, a: &Attachment) -> AnyElement {
    let url = a.url();
    div()
        .id(SharedString::from(format!("bubble-file-{id}")))
        .underline()
        .text_color(SeancePalette::violet())
        .cursor_pointer()
        .on_click(move |_, _, _| crate::sysopen::open_detached(&url))
        .child(a.title.clone())
        .into_any_element()
}

/// A bubble's right-click menu. It reads the circle list when it opens, so
/// no bubble holds a copy of it.
fn bubble_menu(
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
    app: &WeakEntity<SeanceApp>,
    ws: &str,
    m: &Msg,
) -> PopupMenu {
    let text = m.text.clone();
    let mut menu = menu.item(PopupMenuItem::new("Copy text").on_click(move |_, _, cx| {
        let _ = crate::clipboard::copy_text_to_clipboard(&text, cx);
    }));
    if m.id > 0 {
        let id = m.id;
        let (from, weak) = (ws.to_string(), app.clone());
        menu = menu.submenu("Forward to", window, cx, move |menu, _, cx| {
            let circles = weak
                .read_with(cx, |a, _| a.chat.circles.clone())
                .unwrap_or_default();
            circles
                .into_iter()
                .filter(|c| c.ws != from && c.ws != ALERTS_WS)
                .fold(menu, |menu, c| {
                    let (weak, to, name) = (weak.clone(), c.ws.clone(), c.name.clone());
                    menu.item(PopupMenuItem::new(c.name).on_click(move |_, window, cx| {
                        let (to, name, win) = (to.clone(), name.clone(), window.window_handle());
                        let _ = weak.update(cx, |a, cx| {
                            a.chat_call(
                                move || chat_api::forward(id, &to),
                                move |_, cx| toast(win, cx, format!("Forwarded to {name}")),
                                cx,
                            )
                        });
                    }))
                })
        });
        for (lead, title) in [
            (false, "Remind me"),
            (true, "Have the lead pick this up later"),
        ] {
            if lead && ws == ALERTS_WS {
                continue;
            }
            let weak = app.clone();
            menu = menu.submenu(title, window, cx, move |menu, _, _| {
                let times = remind_times(now_ms());
                REMIND_CHOICES
                    .iter()
                    .zip(times)
                    .fold(menu, |menu, ((shown, said), at)| {
                        let weak = weak.clone();
                        menu.item(PopupMenuItem::new(*shown).on_click(move |_, window, cx| {
                            let win = window.window_handle();
                            let _ = weak.update(cx, |a, cx| {
                                a.chat_call(
                                    move || chat_api::remind(id, at, lead, said),
                                    move |_, cx| {
                                        let who = if lead {
                                            "The lead will pick it up"
                                        } else {
                                            "Reminder set:"
                                        };
                                        toast(win, cx, format!("{who} {said}"))
                                    },
                                    cx,
                                )
                            });
                        }))
                    })
            });
        }
    }
    let (weak, ws) = (app.clone(), ws.to_string());
    menu.item(PopupMenuItem::new("Mark unread").on_click(move |_, _, cx| {
        let ws = ws.clone();
        let _ = weak.update(cx, |a, cx| a.chat_unread(ws, cx));
    }))
}

impl SeanceApp {
    pub(super) fn start_chat_poll(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let Some(this) = this.upgrade() else { break };
            let (on, ws, after) = this.update(cx, |app: &mut SeanceApp, _| {
                let ws = app.chat.selected.clone();
                let after = ws.as_deref().and_then(|w| app.chat.refresh_after(w));
                (app.view.mode == ViewMode::Chat, ws, after)
            });
            if !on {
                cx.background_executor().timer(IDLE).await;
                continue;
            }
            let round = cx
                .background_executor()
                .spawn(async move { fetch_round(ws, after) })
                .await;
            let failed = round.error.is_some();
            let v = this.update(cx, |app: &mut SeanceApp, cx| {
                app.apply_round(round, cx);
                app.chat.v
            });
            if failed {
                cx.background_executor().timer(RETRY).await;
                continue;
            }
            // Returns on a change, or when the wait runs out; either way the
            // next round reads Invoke again.
            let polled = cx
                .background_executor()
                .spawn(async move { chat_api::poll(v, POLL_SECS) })
                .await;
            if polled.is_err() {
                cx.background_executor().timer(RETRY).await;
            }
        })
        .detach();
    }

    fn apply_round(&mut self, round: Round, cx: &mut Context<Self>) {
        let chat = &mut self.chat;
        chat.error = round.error;
        if let Some(c) = round.circles {
            chat.v = c.v;
            chat.circles = c.circles;
        }
        if chat.selected.is_none() {
            chat.stick = 3;
            chat.selected = self
                .selected_workspace
                .clone()
                .filter(|ws| chat.circles.iter().any(|c| &c.ws == ws))
                .or_else(|| chat.circles.first().map(|c| c.ws.clone()));
        }
        if let Some((ws, p)) = round.progress {
            chat.progress.insert(ws, p);
        }
        if let Some((ws, t, delta)) = round.thread {
            self.apply_thread(ws, t, delta, cx);
        }
        cx.notify();
    }

    /// Take in a thread answer. A `delta` holds only the messages past what
    /// we had; they join it, and bubbles shown before Invoke had them go.
    fn apply_thread(
        &mut self,
        ws: String,
        mut thread: Thread,
        delta: bool,
        cx: &mut Context<Self>,
    ) {
        let chat = &mut self.chat;
        let before = chat.last_id(&ws);
        if delta {
            let mut merged = chat.threads.remove(&ws).unwrap_or_default().messages;
            merged.retain(|m| m.id > 0);
            for m in std::mem::take(&mut thread.messages) {
                match merged.iter_mut().find(|o| o.id == m.id) {
                    Some(o) => *o = m,
                    None => merged.push(m),
                }
            }
            thread.messages = merged;
        }
        let newest = thread.messages.iter().map(|m| m.id).max().unwrap_or(0);
        // New messages follow the bottom, but never pull him away from older
        // ones he is reading.
        if before != Some(newest) && !chat.unpinned && chat.no_pin.as_deref() != Some(ws.as_str()) {
            chat.stick = 3;
        }
        // Images the thread shows: fetch each once.
        let names: Vec<String> = thread
            .messages
            .iter()
            .flat_map(|m| m.image_names().into_iter().map(str::to_string))
            .filter(|n| !is_video(n))
            .collect();
        for name in names {
            if chat.image_asked.insert(name.clone()) {
                cx.spawn(async move |this, cx| {
                    let leaf = name.clone();
                    let got = cx
                        .background_executor()
                        .spawn(async move {
                            // Through Invoke's /api/file, into the cache; its
                            // size reserves the bubble's room before it paints.
                            chat_api::file(&leaf).map(|p| {
                                let (w, h) = chat_api::image_size(&p).unwrap_or((0, 0));
                                (p, w, h)
                            })
                        })
                        .await;
                    let Some(this) = this.upgrade() else { return };
                    this.update(cx, |app: &mut SeanceApp, cx| {
                        match got {
                            Ok(found) => {
                                app.chat.images.insert(name, found);
                            }
                            // Let a later thread refresh try again.
                            Err(_) => {
                                app.chat.image_asked.remove(&name);
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        chat.usage = thread.usage.clone();
        chat.threads.insert(ws, thread);
    }

    /// Reading the open thread tells Invoke, so the phone's badge clears: only
    /// while this window is the active one and Chat is showing that thread.
    fn maybe_mark_read(&mut self, cx: &mut Context<Self>) {
        let chat = &mut self.chat;
        let Some(ws) = chat.selected.clone() else {
            return;
        };
        if self.view.mode != ViewMode::Chat
            || !chat.window_active
            || chat.read_busy.contains(&ws)
            || chat.hold_read.as_deref() == Some(ws.as_str())
        {
            return;
        }
        if chat
            .read_retry_at
            .get(&ws)
            .is_some_and(|at| Instant::now() < *at)
        {
            return;
        }
        let Some(thread) = chat.threads.get(&ws) else {
            return;
        };
        let Some(newest) = chat.last_id(&ws) else {
            return;
        };
        let done = thread
            .read
            .max(chat.read_sent.get(&ws).copied().unwrap_or(0));
        if newest <= done {
            return;
        }
        chat.read_busy.insert(ws.clone());
        cx.spawn(async move |this, cx| {
            let at = ws.clone();
            let sent = cx
                .background_executor()
                .spawn(async move { chat_api::mark_read(&at, newest) })
                .await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, _| {
                app.chat.read_busy.remove(&ws);
                // Only a report Invoke accepted counts; a failed one is retried.
                match sent {
                    // Not while the circle is marked unread: that report is stale.
                    Ok(()) if app.chat.hold_read.as_deref() == Some(ws.as_str()) => {}
                    Ok(()) => {
                        app.chat.read_sent.insert(ws, newest);
                    }
                    // Not every render: a refusal that never clears would
                    // otherwise run curl each frame.
                    Err(_) => {
                        app.chat
                            .read_retry_at
                            .insert(ws, Instant::now() + READ_RETRY);
                    }
                }
            });
        })
        .detach();
    }

    fn chat_select(&mut self, ws: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.selected.as_deref() != Some(ws) {
            self.chat.selected = Some(ws.to_string());
            self.tasks_popover = None;
        }
        // Opening a circle, or clicking the open one, shows its newest message.
        self.chat.arrive();
        self.chat.jump = None;
        self.chat.notice = None;
        // Selecting a circle again is what reads one marked unread.
        if self.chat.hold_read.as_deref() == Some(ws) {
            self.chat.hold_read = None;
        }
        // The lead's to-do poll follows the selected circle.
        if self.workspaces().iter().any(|w| w == ws) {
            self.selected_workspace = Some(ws.to_string());
        }
        if let Some(input) = self.chat.input.as_ref() {
            let focus = input.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        self.chat_refresh(cx);
        cx.notify();
    }

    /// Read the selected circle's whole thread again, so no old status is stale.
    fn chat_refresh(&mut self, cx: &mut Context<Self>) {
        let ws = self.chat.selected.clone();
        cx.spawn(async move |this, cx| {
            let round = cx
                .background_executor()
                .spawn(async move { fetch_round(ws, None) })
                .await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| app.apply_round(round, cx));
        })
        .detach();
    }

    /// Run one Invoke call off the UI thread. `then` runs when it went
    /// through; a refusal shows as the notice, in Invoke's own words.
    fn chat_call(
        &mut self,
        job: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.chat.notice = None;
        cx.spawn(async move |this, cx| {
            let done = cx.background_executor().spawn(async move { job() }).await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                match done {
                    Ok(()) => then(app, cx),
                    Err(e) => {
                        app.chat.hold_read = None;
                        app.chat.notice = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stop the open circle's lead mid-turn.
    fn chat_stop(&mut self, ws: String, cx: &mut Context<Self>) {
        self.chat_call(move || chat_api::stop(&ws), Self::chat_refresh, cx);
    }

    /// Mark a circle unread. An open thread would read itself again at once,
    /// so it is held back until the circle is selected again.
    fn chat_unread(&mut self, ws: String, cx: &mut Context<Self>) {
        let at = ws.clone();
        // Held before the call goes out, so no read can slip in behind it; a
        // refusal lifts the hold again (see `chat_call`).
        if self.chat.selected.as_deref() == Some(ws.as_str()) {
            self.chat.hold_read = Some(ws.clone());
        }
        self.chat_call(
            move || chat_api::unread(&at),
            move |app, cx| {
                app.chat.read_sent.remove(&ws);
                app.chat_refresh(cx);
            },
            cx,
        );
    }

    /// A click on the header's creature: a dance move and a line, or at the
    /// fifth click in a row a party. The bubble's frames come from a short
    /// timer, so nothing else has to be re-rendering; the party asks for its
    /// own in `render_chat`.
    fn chat_poke(&mut self, ws: String, cx: &mut Context<Self>) {
        use super::sprite::{Poke, LINE_MS, PARTY_LINE, PARTY_MS};
        let now = now_ms();
        // More clicks while a party runs must not end it by replacing it with
        // a dance and a line.
        if self.chat.party.as_ref().is_some_and(|p| p.on(now)) {
            eprintln!("[seance chat] creature poke ws={ws} at={now} -> ignored, party running");
            return;
        }
        let (mv, line) = match self.chat.pokes.poke(now) {
            Poke::Dance { mv, line } => (Some(mv), line),
            Poke::Party => {
                super::sprite::start_party(now);
                let others = self
                    .chat
                    .circles
                    .iter()
                    .filter(|c| c.ws != ws)
                    .map(|c| super::sprite::for_circle(&c.ws));
                self.chat.party = Some(super::sprite::Party::new(
                    super::sprite::for_circle(&ws),
                    others,
                    now,
                    &mut rand::thread_rng(),
                ));
                // Frames stop while the window is inactive; this redraw
                // takes the scene down on time anyway.
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(PARTY_MS))
                        .await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
                (None, PARTY_LINE)
            }
        };
        // Kept in the GUI log (~/.local/share/seance/gui.stderr.log) so a
        // report that the egg did nothing can be checked.
        eprintln!(
            "[seance chat] creature poke ws={ws} at={now} {}",
            if mv.is_none() { "-> PARTY" } else { "-> dance" }
        );
        self.chat.egg = Some(Egg {
            ws,
            mv,
            at: now,
            line,
        });
        cx.spawn(async move |this, cx| {
            let end = now_ms() + LINE_MS;
            while now_ms() < end {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
            }
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    /// Send `/compact` to the open circle's lead, as the composer would.
    fn chat_compact(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = self.chat.selected.clone() else {
            return;
        };
        if self.compact_locked(&ws) || ws == ALERTS_WS {
            return;
        }
        self.chat.compact_at = Some((ws.clone(), Instant::now()));
        // The button comes back on its own once the wait is over.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMPACT_COOLDOWN).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        self.chat_post(ws, "/compact".to_string(), None, false, cx);
    }

    fn compact_locked(&self, ws: &str) -> bool {
        self.chat
            .compact_at
            .as_ref()
            .is_some_and(|(at_ws, at)| at_ws == ws && at.elapsed() < COMPACT_COOLDOWN)
    }

    /// The Compact button beside a high ctx; dimmed and dead after a click.
    fn compact_button(&self, ws: &str, cx: &Context<Self>) -> AnyElement {
        let locked = self.compact_locked(ws);
        div()
            .id("chat-compact")
            .underline()
            .text_color(SeancePalette::text_dim())
            .tooltip(tip("send /compact to the lead"))
            .when(locked, |d| d.opacity(0.4))
            .when(!locked, |d| {
                d.hover(|s| s.text_color(SeancePalette::flame()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.chat_compact(cx)))
            })
            .child("Compact")
            .into_any_element()
    }

    /// Search once typing has paused, and only for two characters or more.
    fn chat_search_soon(&mut self, cx: &mut Context<Self>) {
        let seq = self.chat.search_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                if app.chat.search_seq == seq {
                    app.chat_search(cx);
                }
            });
        })
        .detach();
    }

    /// Search every thread; the results replace the circle list.
    fn chat_search(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.chat.search.clone() else {
            return;
        };
        let q = match search_query(&input.read(cx).value()) {
            Ok(q) => q.to_string(),
            Err(hint) => {
                self.chat.hits = None;
                self.chat.search_note = hint.map(str::to_string);
                cx.notify();
                return;
            }
        };
        self.chat.search_note = None;
        cx.spawn(async move |this, cx| {
            let asked = q.clone();
            let found = cx
                .background_executor()
                .spawn(async move { chat_api::search(&asked) })
                .await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                // An answer for words that have since changed is dropped.
                let now = app.chat.search.as_ref().map(|i| i.read(cx).value());
                if now.as_deref().map(str::trim) == Some(q.as_str()) {
                    match found {
                        Ok(hits) => {
                            if hits.is_empty() {
                                app.chat.search_note = Some("No results".into());
                            }
                            app.chat.hits = Some(hits);
                        }
                        // Beside the box: no thread need be open to see it.
                        Err(e) => app.chat.search_note = Some(format!("Search failed: {e}")),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open a search result: its circle, scrolled to the message.
    fn chat_open_hit(&mut self, ws: &str, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_select(ws, window, cx);
        self.chat.jump = Some(id);
    }

    /// The composer changed: an `@` first asks for the circle's panes.
    fn chat_input_changed(&mut self, cx: &mut Context<Self>) {
        let (Some(ws), Some(input)) = (self.chat.selected.clone(), self.chat.input.clone()) else {
            return;
        };
        if input.read(cx).value().starts_with('@')
            && !self
                .chat
                .panes_at
                .get(&ws)
                .is_some_and(|at| at.elapsed() < PANES_TTL)
        {
            self.chat.panes_at.insert(ws.clone(), Instant::now());
            cx.spawn(async move |this, cx| {
                let asked = ws.clone();
                let found = cx
                    .background_executor()
                    .spawn(async move { chat_api::panes(&asked) })
                    .await;
                let Some(this) = this.upgrade() else { return };
                this.update(cx, |app: &mut SeanceApp, cx| {
                    match found {
                        Ok(panes) => {
                            app.chat.panes.insert(ws, panes);
                        }
                        Err(e) => {
                            app.chat.panes_at.remove(&ws);
                            app.chat.notice = Some(e.to_string());
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Pick a pane from the `@` suggestions.
    fn chat_mention(&mut self, at: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.chat.input.clone() {
            input.update(cx, |i, cx| i.set_value(format!("@{at} "), window, cx));
            let focus = input.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    /// Pick a file to send as a note, with whatever is typed as its caption.
    fn chat_attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |app, window, cx| {
                if let Err(e) = chat_api::check_upload(&path) {
                    app.chat.notice = Some(e.to_string());
                    cx.notify();
                    return;
                }
                let (Some(ws), Some(input)) = (app.chat.selected.clone(), app.chat.input.clone())
                else {
                    return;
                };
                let caption = input.read(cx).value().trim().to_string();
                input.update(cx, |i, cx| i.set_value("", window, cx));
                app.chat_post(ws, caption, Some(path), true, cx);
            });
        })
        .detach();
    }

    fn chat_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(ws), Some(input)) = (self.chat.selected.clone(), self.chat.input.clone()) else {
            return;
        };
        let text = input.read(cx).value().trim().to_string();
        if ws == ALERTS_WS || text.is_empty() {
            return;
        }
        input.update(cx, |i, cx| i.set_value("", window, cx));
        self.chat_post(ws, text, None, true, cx);
    }

    /// Send a note to `ws`'s lead, with a file if there is one. Shown at once;
    /// the next thread answer replaces it with the real one. `typed` says the
    /// words came from the composer, so a failed send puts them back there.
    fn chat_post(
        &mut self,
        ws: String,
        text: String,
        file: Option<PathBuf>,
        typed: bool,
        cx: &mut Context<Self>,
    ) {
        let shown = match (&file, text.is_empty()) {
            (Some(path), true) => format!(
                "📎 {}",
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
            _ => text.clone(),
        };
        self.chat.next_local -= 1;
        let local_id = self.chat.next_local;
        let after = self.chat.refresh_after(&ws);
        let thread = self.chat.threads.entry(ws.clone()).or_default();
        thread.messages.push(Msg {
            id: local_id,
            from: "me".into(),
            text: shown,
            ts: now_ms(),
            status: "sending".into(),
            ..Default::default()
        });
        self.chat.arrive();
        let thread_ws = ws.clone();
        cx.spawn(async move |this, cx| {
            let to = ws.clone();
            let body = text.clone();
            let sent = cx
                .background_executor()
                .spawn(async move {
                    match file {
                        Some(path) => chat_api::send_file(&to, &body, &path),
                        None => chat_api::send(&to, &body),
                    }
                })
                .await;
            let round = cx
                .background_executor()
                .spawn(async move { fetch_round(Some(ws), after) })
                .await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                app.apply_round(round, cx);
                if let Err(e) = sent {
                    // Not sent: the bubble goes and the words go back where
                    // they were typed.
                    if let Some(t) = app.chat.threads.get_mut(&thread_ws) {
                        t.messages.retain(|m| m.id != local_id);
                    }
                    app.chat.error = Some(format!("not sent: {e}"));
                    if typed && !text.is_empty() {
                        app.chat.restore = Some(text);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Tap a choice: it goes to the lead as your message, tied to the message
    /// it answers. The pill is filled at once and every pill goes dead; if the
    /// send fails they come back, with the reason on top.
    fn chat_answer(&mut self, answers: i64, choice: String, cx: &mut Context<Self>) {
        let Some(ws) = self.chat.selected.clone() else {
            return;
        };
        if ws == ALERTS_WS || self.chat.tapped.contains_key(&answers) {
            return;
        }
        self.chat.tapped.insert(answers, choice.clone());
        // Shown at once as your message, until the thread answer has the real one.
        self.chat.next_local -= 1;
        let local_id = self.chat.next_local;
        let after = self.chat.refresh_after(&ws);
        let thread = self.chat.threads.entry(ws.clone()).or_default();
        thread.messages.push(Msg {
            id: local_id,
            from: "me".into(),
            text: choice.clone(),
            ts: now_ms(),
            status: "sending".into(),
            ..Default::default()
        });
        self.chat.arrive();
        let thread_ws = ws.clone();
        cx.spawn(async move |this, cx| {
            let (to, text) = (ws.clone(), choice);
            let sent = cx
                .background_executor()
                .spawn(async move { chat_api::answer(&to, &text, answers) })
                .await;
            let round = cx
                .background_executor()
                .spawn(async move { fetch_round(Some(ws), after) })
                .await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                app.apply_round(round, cx);
                if let Err(e) = sent {
                    // Not sent: the pills are live again and the bubble goes.
                    app.chat.tapped.remove(&answers);
                    if let Some(t) = app.chat.threads.get_mut(&thread_ws) {
                        t.messages.retain(|m| m.id != local_id);
                    }
                    app.chat.error = Some(format!("not sent: {e}"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Back to Classic, on this circle's lead.
    fn chat_show_terminal(&mut self, ws: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_view(ViewMode::Classic, cx);
        self.selected_workspace = Some(ws.to_string());
        if let Some(lead) = self.stage_lead(ws) {
            self.set_active(&lead, window, cx);
        }
        cx.notify();
    }

    /// Is the human typing in the composer or the search box? The focus pass
    /// must leave them be: it hands focus back to a terminal from any handle
    /// it does not know, which kept the search box from holding a keystroke.
    pub(super) fn chat_input_focused(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.view.mode == ViewMode::Chat
            && [&self.chat.input, &self.chat.search]
                .into_iter()
                .flatten()
                .any(|i| {
                    window
                        .focused(cx)
                        .is_some_and(|f| f == i.read(cx).focus_handle(cx))
                })
    }

    pub(super) fn render_chat(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.chat.input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .auto_grow(1, 6)
                    .submit_on_enter(true)
                    .placeholder("Message the lead")
            });
            cx.subscribe_in(
                &input,
                window,
                |this: &mut SeanceApp, _input, event: &InputEvent, window, cx| {
                    // Enter sends; Shift+Enter adds a line (the input does that).
                    match event {
                        InputEvent::PressEnter { shift: false, .. } => this.chat_send(window, cx),
                        // Quick replies and @ suggestions follow what is typed.
                        InputEvent::Change => this.chat_input_changed(cx),
                        _ => {}
                    }
                },
            )
            .detach();
            let focus = input.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            self.chat.input = Some(input);
        }
        if self.chat.search.is_none() {
            let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search chats"));
            cx.subscribe_in(
                &search,
                window,
                |this: &mut SeanceApp, input, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => {
                        // Enter searches now; a pending timer must not repeat it.
                        this.chat.search_seq += 1;
                        this.chat_search(cx);
                    }
                    InputEvent::Change => {
                        this.chat.search_seq += 1;
                        if input.read(cx).value().trim().is_empty() {
                            this.chat.hits = None;
                            this.chat.search_note = None;
                            cx.notify();
                        } else {
                            this.chat_search_soon(cx);
                        }
                    }
                    _ => {}
                },
            )
            .detach();
            self.chat.search = Some(search);
        }
        self.chat.window_active = window.is_window_active();
        // Set again as the header's creature is painted, if it still is.
        self.chat.creature_at.set(None);
        if let Some(input) = self.chat.input.clone() {
            if let Some(text) = self.chat.restore.take() {
                // Whatever was typed since stays, after the failed text.
                let typed = input.read(cx).value().trim().to_string();
                let text = if typed.is_empty() {
                    text
                } else {
                    format!("{text}\n{typed}")
                };
                input.update(cx, |i, cx| i.set_value(text, window, cx));
            }
            if std::mem::take(&mut self.chat.focus_input) {
                let focus = input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
        }
        self.maybe_mark_read(cx);
        // A search result waits here for its thread to arrive; it takes the
        // frame from the pin to the bottom.
        if let Some(id) = self.chat.jump {
            let at = self
                .chat
                .selected
                .as_ref()
                .and_then(|ws| self.chat.threads.get(ws))
                .and_then(|t| t.messages.iter().position(|m| m.id == id));
            if let Some(at) = at {
                self.chat.jump = None;
                self.chat.stick = 0;
                self.chat.no_pin = self.chat.selected.clone();
                self.chat.scroll.scroll_to_item(at);
                window.request_animation_frame();
            }
        }
        // Follow the bottom while pinned: images, chips and new messages change
        // the thread's size after the pin frames are over.
        let now = (
            f32::from(self.chat.scroll.offset().y),
            f32::from(self.chat.scroll.max_offset().y),
        );
        let held = self.chat.no_pin.is_some() || self.chat.jump.is_some();
        let (pinned, down) = follow_step(!self.chat.unpinned, held, self.chat.seen, now);
        self.chat.unpinned = !pinned;
        self.chat.seen = Some(now);
        if self.chat.stick > 0 {
            self.chat.stick -= 1;
            self.chat.scroll.scroll_to_bottom();
            window.request_animation_frame();
        } else if down {
            self.chat.scroll.scroll_to_bottom();
            window.request_animation_frame();
        }
        // Crews dance smoothly, so they need every frame while one shows in
        // the header or a list row in view; the spinner's 240ms tick is too
        // coarse. Not in a window nobody is looking at: rebuilding the view 60
        // times a second there would only burn battery.
        let items = self.chat_list_items();
        let now = now_ms();
        // Every circle, not just the rows shown, so a search keeps the dances.
        let crews = self
            .chat
            .circles
            .iter()
            .map(|c| (c.ws.as_str(), &c.crew[..]));
        super::sprite::keep_crew_dances(&mut self.chat.list_dances, crews, now);
        // The list's children in view as of its last layout; all of them
        // before it has one.
        let list = &self.chat.list_scroll;
        let seen = if list.bounds_for_item(0).is_some() {
            list.top_item()..=list.bottom_item()
        } else {
            0..=usize::MAX
        };
        self.chat.list_dancing.clear();
        for (i, item) in items.iter().enumerate() {
            if let ListItem::Circle(c) = item {
                if let Some(d) = self.chat.list_dances.get_mut(&c.ws) {
                    if seen.contains(&i) {
                        d.advance(now, &mut rand::thread_rng());
                        self.chat.list_dancing.insert(c.ws.clone());
                    }
                }
            }
        }
        let crew = self
            .chat
            .selected
            .as_ref()
            .and_then(|ws| self.chat.threads.get(ws))
            .is_some_and(|t| super::sprite::is_crew(&t.crew));
        if crew {
            self.chat
                .crew_dance
                .get_or_insert_with(|| super::sprite::CrewDance::new(now))
                .advance(now, &mut rand::thread_rng());
        } else {
            self.chat.crew_dance = None;
        }
        self.chat.party = self.chat.party.take().filter(|p| p.on(now));
        if let Some(party) = self.chat.party.as_mut() {
            party.advance(now, &mut rand::thread_rng());
        }
        let party = self.chat.party.is_some();
        if (crew || party || !self.chat.list_dancing.is_empty()) && self.chat.window_active {
            window.request_animation_frame();
        }
        let list = self.render_chat_list(items, cx);
        let thread = self.render_chat_thread(cx);
        div()
            .id("chat")
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .bg(SeancePalette::bg())
            .child(list)
            .child(thread)
            .children(self.party_overlay(window.viewport_size(), cx))
            .into_any_element()
    }

    /// Circles in the sidebar's order: grouped by space, as Classic lists them.
    fn chat_circles_in_order(&self) -> Vec<CircleRow> {
        let local = self.subs_pref.spaces.all_circles(&self.workspaces());
        let at = |ws: &str| local.iter().position(|l| l == ws).unwrap_or(usize::MAX);
        let mut rows = self.chat.circles.clone();
        rows.sort_by_key(|c| at(&c.ws));
        rows
    }

    /// The circle list's children in order: none while search results stand
    /// in for the circles.
    fn chat_list_items(&self) -> Vec<ListItem> {
        let mut items = Vec::new();
        if self.chat.hits.is_some() {
            return items;
        }
        let spaces = &self.subs_pref.spaces;
        // Active circles sit above the spaces, lifted out of theirs.
        let active = active_circles(&self.chat.circles, now_ms());
        if !active.is_empty() {
            items.push(ListItem::Label("ACTIVE".to_string()));
            items.extend(active.iter().cloned().map(ListItem::Circle));
        }
        let mut space = String::new();
        for c in self.chat_circles_in_order() {
            if active.iter().any(|a| a.ws == c.ws) {
                continue;
            }
            let id = spaces.space_of(&c.ws).to_string();
            if id != space {
                if let Some(s) = spaces.list.iter().find(|s| s.id == id) {
                    items.push(ListItem::Label(s.name.to_uppercase()));
                }
                space = id;
            }
            items.push(ListItem::Circle(c));
        }
        items
    }

    fn render_chat_list(&self, items: Vec<ListItem>, cx: &Context<Self>) -> AnyElement {
        let mut col = div()
            .id("chat-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.chat.list_scroll)
            .flex()
            .flex_col();
        if let Some(hits) = &self.chat.hits {
            // Search results stand in for the circles.
            if !hits.is_empty() {
                col = col.child(list_label(format!("{} FOUND", hits.len())));
            }
            for h in hits {
                col = col.child(self.chat_hit_row(h, cx));
            }
        }
        for item in items {
            col = col.child(match item {
                ListItem::Label(text) => list_label(text).into_any_element(),
                ListItem::Circle(c) => self.chat_circle_row(&c, cx),
            });
        }
        div()
            .w(px(LIST_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(SeancePalette::bg_elevated())
            .border_r_1()
            .border_color(SeancePalette::border())
            .child(
                div()
                    .flex_none()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            // The ✦ opens the same menu as in Classic and Stage:
                            // the connected guis and the theme list.
                            .child(self.render_seance_mark("chat", cx))
                            // The AFK chip as Stage and Wall draw it; nothing
                            // where the script is not installed.
                            .children(self.render_afk_chip("chat", cx)),
                    )
                    .child(self.render_view_switch("chat", cx))
                    .children(self.chat.search.as_ref().map(|s| Input::new(s)))
                    .children(self.chat.search_note.as_ref().map(|n| {
                        div()
                            .text_xs()
                            .text_color(SeancePalette::text_faint())
                            .child(n.clone())
                    })),
            )
            .child(col)
            .into_any_element()
    }

    /// One search result: the circle and who spoke, then the words.
    fn chat_hit_row(&self, h: &Hit, cx: &Context<Self>) -> AnyElement {
        let (ws, id) = (h.ws.clone(), h.id);
        let who = if h.from == "me" {
            "you"
        } else {
            h.from.as_str()
        };
        div()
            .id(SharedString::from(format!("chat-hit-{}-{}", h.ws, h.id)))
            .flex_none()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .cursor_pointer()
            .hover(|s| s.bg(SeancePalette::surface()))
            .on_click(
                cx.listener(move |this, _, window, cx| this.chat_open_hit(&ws, id, window, cx)),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child(format!("{} · {who} · {}", h.circle, clock(h.ts))),
            )
            .child(
                div()
                    .line_clamp(2)
                    .text_sm()
                    .text_color(SeancePalette::text())
                    .child(h.text.clone()),
            )
            .into_any_element()
    }

    fn chat_circle_row(&self, c: &CircleRow, cx: &Context<Self>) -> AnyElement {
        let on = self.chat.selected.as_deref() == Some(c.ws.as_str());
        let ws = c.ws.clone();
        let (menu_ws, weak) = (c.ws.clone(), cx.weak_entity());
        let pulse = 0.45 + 0.55 * (((now_ms() / 300) % 2) as f32);
        div()
            .id(SharedString::from(format!("chat-circle-{}", c.ws)))
            .flex_none()
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .when(on, |d| d.bg(SeancePalette::surface()))
            .hover(|s| s.bg(SeancePalette::surface()))
            .on_click(cx.listener(move |this, _, window, cx| this.chat_select(&ws, window, cx)))
            .context_menu(move |menu, _, _| {
                let (ws, weak) = (menu_ws.clone(), weak.clone());
                menu.item(PopupMenuItem::new("Mark unread").on_click(move |_, _, cx| {
                    let ws = ws.clone();
                    let _ = weak.update(cx, |a, cx| a.chat_unread(ws, cx));
                }))
            })
            .child(self.chat_row_creature(c))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(SeancePalette::text())
                                    .child(c.name.clone()),
                            )
                            .when(c.working, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(SeancePalette::flame().opacity(pulse)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(SeancePalette::text_faint())
                            .child(c.last.as_ref().map(preview).unwrap_or_default()),
                    ),
            )
            .when(c.unread > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded_full()
                        .bg(SeancePalette::flame())
                        .text_xs()
                        .text_color(SeancePalette::bg())
                        .child(c.unread.to_string()),
                )
            })
            .into_any_element()
    }

    /// The party over the Chat view: dimmed, with a purple glow, the crew
    /// spinning in the middle and confetti falling. A click on it ends the
    /// party early; it covers the view, so nothing under it takes the click.
    fn party_overlay(
        &self,
        view: gpui::Size<gpui::Pixels>,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        use super::sprite::{confetti_at, party_scene, CONFETTI, GENIE};
        let party = self.chat.party.as_ref()?;
        let now = now_ms();
        let t = now.saturating_sub(party.at);
        let scene = party_scene(t)?;
        let (w, h) = (f32::from(view.width), f32::from(view.height));
        let confetti = (0..CONFETTI).filter_map(|i| {
            let (x, y) = confetti_at(party.at, i, t, w, h)?;
            Some(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .size(px(6.))
                    .bg(party.confetti_color(i)),
            )
        });
        // gpui has no radial gradient: nested translucent discs instead.
        let genie = gpui::Hsla::from(gpui::rgb(GENIE)).opacity(0.13);
        let glow = [0.45, 0.7, 1.].into_iter().fold(div(), |inner, k| {
            let d = w.min(h) * k;
            div()
                .flex_none()
                .size(px(d))
                .rounded_full()
                .bg(genie)
                .flex()
                .items_center()
                .justify_center()
                .child(inner)
        });
        let centred = || {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
        };
        Some(
            div()
                .id("chat-party")
                .absolute()
                .inset_0()
                .overflow_hidden()
                .occlude()
                .opacity(scene.fade)
                .bg(gpui::black().opacity(0.6))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    this.chat_party_click(ev.position(), cx)
                }))
                .child(centred().child(glow))
                .child(centred().child(party.render(now, w)))
                .children(confetti)
                .into_any_element(),
        )
    }

    /// A click on the party ends it early, unless it is on the header's
    /// creature.
    fn chat_party_click(&mut self, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let now = now_ms();
        let on_creature = self.chat.creature_at.get().is_some_and(|b| b.contains(&at));
        if super::sprite::click_party(&mut self.chat.party, now, on_creature) {
            super::sprite::stop_party();
            eprintln!("[seance chat] party ended early at={now}");
        } else if on_creature {
            eprintln!("[seance chat] creature poke at={now} -> ignored, party running");
        }
        cx.notify();
    }

    /// A list row's creature, or its crew while two or more dance in view.
    fn chat_row_creature(&self, c: &CircleRow) -> AnyElement {
        let sprite = super::sprite::for_circle(&c.ws);
        let slot = div().flex_none().w(px(30.)).flex().justify_center();
        let dance = self
            .chat
            .list_dances
            .get(&c.ws)
            .filter(|_| self.chat.list_dancing.contains(&c.ws));
        let Some(dance) = dance else {
            return slot
                .child(super::sprite::render(&sprite, 2., c.working))
                .into_any_element();
        };
        let cuts = super::sprite::crew_at(&c.crew, dance, now_ms());
        // Wider than the slot: centred over it in a box that holds it, as
        // tall as the lone creature, so the row and its text stay put.
        slot.h(px(20.))
            .relative()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(-20.))
                    .right(px(-20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(super::sprite::render_crew(&sprite, &cuts, None, LIST_CREW)),
            )
            .into_any_element()
    }

    /// The thread header's creature. Click it: it dances and says something.
    fn header_creature(&self, ws: &str, working: bool, cx: &Context<Self>) -> AnyElement {
        let now = now_ms();
        let egg = self.chat.egg.as_ref().filter(|e| e.ws == ws);
        let moving = egg.and_then(|e| e.mv.map(|mv| (mv, e.at)));
        let say = egg
            .filter(|e| now.saturating_sub(e.at) < super::sprite::LINE_MS)
            .map(|e| e.line);
        let target = ws.to_string();
        let sprite = super::sprite::for_circle(ws);
        // Two or more panes working: a crew of cutouts instead of one creature.
        let crew = self.chat.threads.get(ws).map_or(&[][..], |t| &t.crew);
        let dance = self
            .chat
            .crew_dance
            .unwrap_or_else(|| super::sprite::CrewDance::new(now));
        let cuts = super::sprite::crew_at(crew, &dance, now);
        div()
            .id("chat-creature")
            .relative()
            .flex_none()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.chat_poke(target.clone(), cx)))
            .child({
                let at = self.chat.creature_at.clone();
                gpui::canvas(move |b, _, _| at.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .size_full()
            })
            .child(if cuts.is_empty() {
                super::sprite::render_move(&sprite, 2.5, working, moving)
            } else {
                super::sprite::render_crew(&sprite, &cuts, moving, 1.)
            })
            .children(say.map(|line| {
                // Deferred, so the thread below cannot paint over it.
                gpui::deferred(
                    div()
                        .absolute()
                        .top(px(34.))
                        .left(px(0.))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(SeancePalette::border())
                        .bg(SeancePalette::bg_elevated())
                        .shadow_lg()
                        .text_xs()
                        .text_color(SeancePalette::text())
                        .whitespace_nowrap()
                        .child(line),
                )
                .with_priority(5)
            }))
            .into_any_element()
    }

    fn render_chat_thread(&self, cx: &Context<Self>) -> AnyElement {
        let Some(ws) = self.chat.selected.clone() else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(SeancePalette::text_faint())
                .flex_col()
                .gap_1()
                .child(match &self.chat.error {
                    Some(e) if e == chat_api::NO_URL_MESSAGE => e.clone(),
                    Some(e) => format!("Can't reach Invoke: {e}"),
                    None => "Loading circles…".to_string(),
                })
                // A refusal has to show even before a thread is open.
                .children(
                    self.chat
                        .notice
                        .clone()
                        .map(|n| div().text_xs().text_color(SeancePalette::danger()).child(n)),
                )
                .into_any_element();
        };
        let circle = self.chat.circles.iter().find(|c| c.ws == ws);
        let name = circle.map_or(ws.clone(), |c| c.name.clone());
        let working = circle.is_some_and(|c| c.working);
        let line = self
            .chat
            .progress
            .get(&ws)
            .filter(|p| p.busy)
            .and_then(|p| p.doing.clone())
            .unwrap_or_default();
        let lead = self.stage_lead(&ws);
        let todo = lead
            .as_ref()
            .and_then(|l| self.lead_tasks.get(l))
            .map_or(0, |d| d.open_count());

        let mut header = div()
            .flex_none()
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(SeancePalette::border())
            .child(self.header_creature(&ws, working, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .text_color(SeancePalette::text())
                            .child(name),
                    )
                    .when(!line.is_empty(), |d| {
                        d.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(SeancePalette::flame())
                                .child(line),
                        )
                    }),
            );
        let read_only = ws == ALERTS_WS;
        let tasks_open = lead.as_deref().is_some_and(|l| self.tasks_popover_open(l));
        let chip_lead = lead.clone();
        header = header.when(!read_only, |header| {
            header.child(
                div()
                    .id("chat-todo")
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .text_xs()
                    .text_color(if tasks_open {
                        SeancePalette::bg()
                    } else {
                        SeancePalette::text_dim()
                    })
                    .when(tasks_open, |d| d.bg(SeancePalette::violet()))
                    .hover(|s| s.text_color(SeancePalette::flame()))
                    .cursor_pointer()
                    .tooltip(tip("the lead's tasks"))
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                        if let Some(lead) = chip_lead.as_deref() {
                            this.toggle_tasks_popover(lead, ev.position(), cx);
                        }
                    }))
                    .child(format!("☑ {todo}")),
            )
        });
        header = header.when(!read_only, |header| {
            header.child(
                div()
                    .id("chat-terminal")
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .text_xs()
                    .text_color(SeancePalette::text_dim())
                    .bg(SeancePalette::surface())
                    .hover(|s| s.text_color(SeancePalette::flame()))
                    .cursor_pointer()
                    .tooltip(tip("flip to Classic, on this circle's lead"))
                    .on_click({
                        let ws = ws.clone();
                        cx.listener(move |this, _, window, cx| {
                            this.chat_show_terminal(&ws, window, cx)
                        })
                    })
                    .child("show terminal"),
            )
        });

        let mut thread = div()
            .id("chat-thread")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.chat.scroll)
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2();
        let messages = self.chat.threads.get(&ws).map(|t| t.messages.as_slice());
        // The last message the user has read, on any device: the lead's messages up
        // to it show as seen.
        let read = self.chat.threads.get(&ws).map_or(0, |t| t.read);
        for m in messages.unwrap_or_default() {
            thread = thread.child(self.chat_bubble(m, read, cx));
        }
        if messages.is_some_and(<[Msg]>::is_empty) {
            thread = thread.child(
                div()
                    .text_sm()
                    .text_color(SeancePalette::text_faint())
                    .child("No messages yet. Say something to the lead."),
            );
        }

        let mut col = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .children(usage_strip(
                self.chat.usage.as_ref(),
                self.chat.threads.get(&ws).and_then(|t| t.context),
                self.chat
                    .threads
                    .get(&ws)
                    .and_then(|t| t.context)
                    .filter(|c| *c >= CTX_ORANGE && !read_only)
                    .map(|_| self.compact_button(&ws, cx)),
            ));
        if let Some(e) = &self.chat.error {
            col = col.child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(SeancePalette::danger())
                    .child(format!("Invoke: {e}")),
            );
        }
        if let Some(n) = &self.chat.notice {
            col = col.child(
                div()
                    .id("chat-notice")
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(SeancePalette::danger())
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.chat.notice = None;
                        cx.notify();
                    }))
                    .child(n.clone()),
            );
        }
        col.child(thread)
            .when(!read_only, |col| col.child(self.render_chat_composer(cx)))
            .into_any_element()
    }

    fn render_chat_composer(&self, cx: &Context<Self>) -> AnyElement {
        let Some(input) = self.chat.input.as_ref() else {
            return div().into_any_element();
        };
        let typed = input.read(cx).value().to_string();
        let ws = self.chat.selected.clone().unwrap_or_default();
        let chip = |id: SharedString| {
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(SeancePalette::border())
                .text_xs()
                .text_color(SeancePalette::text_dim())
                .hover(|s| s.text_color(SeancePalette::flame()))
                .cursor_pointer()
        };
        // Above the input: quick replies while it is empty, or the panes an
        // `@` at the start can name.
        let mut above = div().flex().flex_wrap().gap_1p5();
        // Stop ends the row while the lead works, whatever is typed, but gives
        // way to the @ suggestions.
        let mentioning = typed
            .strip_prefix('@')
            .is_some_and(|t| !t.contains(char::is_whitespace));
        let lead_busy = self.chat.threads.get(&ws).is_some_and(|t| t.lead_busy);
        if typed.is_empty() {
            for (i, text) in QUICK_REPLIES.into_iter().enumerate() {
                let ws = ws.clone();
                above = above.child(
                    chip(SharedString::from(format!("chat-quick-{i}")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.chat_post(ws.clone(), text.to_string(), None, false, cx)
                        }))
                        .child(text),
                );
            }
        } else if let Some(typed) = typed
            .strip_prefix('@')
            .filter(|t| !t.contains(char::is_whitespace))
        {
            let typed = typed.to_lowercase();
            let panes = self.chat.panes.get(&ws).map_or(&[][..], Vec::as_slice);
            for (i, p) in panes
                .iter()
                .filter(|p| {
                    p.helper
                        && (p.at.to_lowercase().starts_with(&typed)
                            || p.name.to_lowercase().contains(&typed))
                })
                .take(6)
                .enumerate()
            {
                let at = p.at.clone();
                let state = if p.asleep {
                    " · asleep"
                } else if p.busy {
                    " · busy"
                } else {
                    ""
                };
                above = above.child(
                    chip(SharedString::from(format!("chat-mention-{i}")))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.chat_mention(&at, window, cx)
                        }))
                        .child(format!("@{} {}{state}", p.at, p.name)),
                );
            }
        }
        if lead_busy && !mentioning {
            let stop_ws = ws.clone();
            above = above.child(
                chip(SharedString::from("chat-stop"))
                    .tooltip(tip("stop what the lead is doing"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.chat_stop(stop_ws.clone(), cx)),
                    )
                    .child("Stop"),
            );
        }
        div()
            .flex_none()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(SeancePalette::border())
            .child(above)
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .id("chat-attach")
                            .flex_none()
                            .px_2()
                            .py_1p5()
                            .rounded_md()
                            .text_sm()
                            .text_color(SeancePalette::text_dim())
                            .bg(SeancePalette::surface())
                            .hover(|s| s.text_color(SeancePalette::flame()))
                            .cursor_pointer()
                            .tooltip(tip("send a file or picture"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.chat_attach(window, cx)),
                            )
                            .child("📎"),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(input)))
                    .child(
                        div()
                            .id("chat-send")
                            .flex_none()
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .text_sm()
                            .text_color(SeancePalette::bg())
                            .bg(SeancePalette::flame())
                            .hover(|s| s.bg(SeancePalette::flame_dim()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| this.chat_send(window, cx)))
                            .child("Send"),
                    ),
            )
            .into_any_element()
    }

    /// A "▶ video" tile in a picture's place: the file name, and a click that
    /// downloads it (no automatic retry) and opens it in its app.
    fn video_tile(&self, id: i64, i: usize, name: &str, cx: &Context<Self>) -> AnyElement {
        let state = self.chat.videos.get(name);
        let status = match state {
            Some(VideoState::Downloading) => "downloading…".to_string(),
            Some(VideoState::Failed(why)) => format!("couldn't download: {why}. Click to retry."),
            None => "click to open".to_string(),
        };
        let busy = state == Some(&VideoState::Downloading);
        let file = name.to_string();
        div()
            .id(SharedString::from(format!("bubble-video-{id}-{i}")))
            .flex_none()
            .w(px(IMG_MAX_W))
            .h(px(IMG_MAX_W * 9. / 16.))
            .px_3()
            .rounded_md()
            .overflow_hidden()
            .bg(SeancePalette::bg().opacity(0.4))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(div().text_color(SeancePalette::text()).child("▶ video"))
            .child(
                div()
                    .max_w_full()
                    .truncate()
                    .text_xs()
                    .text_color(SeancePalette::text_dim())
                    .child(name.to_string()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if matches!(state, Some(VideoState::Failed(_))) {
                        SeancePalette::danger()
                    } else {
                        SeancePalette::text_faint()
                    })
                    .child(status),
            )
            .when(!busy, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.open_video(&file, cx)))
            })
            .into_any_element()
    }

    /// Fetch a video through Invoke and open it in its app. One try per click.
    fn open_video(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.chat.videos.get(name) == Some(&VideoState::Downloading) {
            return;
        }
        self.chat
            .videos
            .insert(name.to_string(), VideoState::Downloading);
        cx.notify();
        let name = name.to_string();
        cx.spawn(async move |this, cx| {
            let leaf = name.clone();
            let got = cx
                .background_executor()
                .spawn(async move { chat_api::file_within(&leaf, VIDEO_SECS) })
                .await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |app: &mut SeanceApp, cx| {
                match got {
                    Ok(path) => {
                        app.chat.videos.remove(&name);
                        crate::sysopen::open_detached(&path.to_string_lossy());
                    }
                    Err(e) => {
                        app.chat
                            .videos
                            .insert(name, VideoState::Failed(e.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Me on the right, the lead on the left, anything else centered.
    fn chat_bubble(&self, m: &Msg, read: i64, cx: &Context<Self>) -> AnyElement {
        let me = m.from == "me";
        let system = !me && m.from != "lead";
        let mut body = div().flex().flex_col().gap_0p5();
        // Every picture, stacked above the text, each in a box whose size is
        // fixed up front: the decoded size fitted to the bubble, or a plain
        // box until the file has arrived. Nothing can paint outside it.
        for (i, name) in m.image_names().into_iter().enumerate() {
            if is_video(name) {
                body = body.child(self.video_tile(m.id, i, name, cx));
                continue;
            }
            let found = self.chat.images.get(name);
            let (bw, bh) = found.map_or((IMG_MAX_W, IMG_LOADING_H), |(_, w, h)| fit_image(*w, *h));
            let frame = div()
                .id(SharedString::from(format!("bubble-img-{}-{i}", m.id)))
                .flex_none()
                .w(px(bw))
                .h(px(bh))
                .rounded_md()
                .overflow_hidden()
                .bg(SeancePalette::bg().opacity(0.4));
            body = body.child(match found {
                // Click opens the cached file in its app (Preview).
                Some((path, ..)) => {
                    let file = path.clone();
                    frame
                        .cursor_pointer()
                        .on_click(move |_, _, _| {
                            crate::sysopen::open_detached(&file.to_string_lossy())
                        })
                        .child(
                            img(path.clone())
                                .size_full()
                                .object_fit(gpui::ObjectFit::Contain),
                        )
                }
                None => frame,
            });
        }
        if let Some(a) = &m.attachment {
            body = body.child(attachment_link(m.id, a));
        }
        let spec = m.spec.as_deref().filter(|_| !me);
        if let Some(title) = spec {
            body = body
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(SeancePalette::violet())
                        .child("SPEC"),
                )
                .child(div().font_semibold().child(title.to_string()));
        }
        let shown = if spec.is_some() {
            spec_body(&m.text)
        } else {
            m.text.as_str()
        };
        if !shown.is_empty() && !attachment_only(m) {
            let (text, markdown) = (shown.to_string(), to_markdown(shown));
            body = body.child(
                div()
                    .id(SharedString::from(format!("bubble-text-{}", m.id)))
                    .min_h(px(14.))
                    // Capture phase, so the selection view cannot swallow it;
                    // the selection itself lands after, hence the defer.
                    .capture_any_mouse_up(cx.listener(
                        move |_, ev: &gpui::MouseUpEvent, window, cx| {
                            if ev.button == gpui::MouseButton::Left && ev.click_count == 2 {
                                let text = text.clone();
                                cx.defer_in(window, move |_, window, cx| {
                                    open_path_under_double_click(&text, window, cx);
                                });
                            }
                        },
                    ))
                    .child(
                        TextView::markdown(
                            SharedString::from(format!("bubble-md-{}", m.id)),
                            markdown,
                        )
                        .style(bubble_text_style())
                        .selectable(true),
                    ),
            );
        }
        let stamp = if m.ts > 0 { clock(m.ts) } else { String::new() };
        if system {
            return div()
                .flex()
                .justify_center()
                .text_xs()
                .text_color(SeancePalette::text_faint())
                .child(body)
                .into_any_element();
        }
        let bubble = div()
            .max_w(relative(0.72))
            .px_3()
            .py_2()
            .rounded_lg()
            .text_sm()
            .text_color(SeancePalette::text())
            .bg(if me {
                SeancePalette::violet_dim()
            } else {
                SeancePalette::surface()
            })
            .when(spec.is_some(), |d| {
                d.border_1().border_color(SeancePalette::violet())
            })
            .child(body)
            .child(
                div()
                    .mt_0p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child(stamp)
                    .children(self.tick_marks(m, read)),
            );
        let (weak, ws, msg) = (cx.weak_entity(), self.chat.selected.clone(), m.clone());
        let bubble = bubble
            .id(SharedString::from(format!("bubble-{}", m.id)))
            .context_menu(move |menu, window, cx| {
                let ws = ws.clone().unwrap_or_default();
                bubble_menu(menu, window, cx, &weak, &ws, &msg)
            });
        if m.choices.is_empty() || me {
            return div()
                .flex()
                .when(me, |d| d.justify_end())
                .child(bubble)
                .into_any_element();
        }
        // The lead's choices, as pills under its message.
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_1()
            .child(bubble)
            .child(self.choice_pills(m, cx))
            .into_any_element()
    }

    /// Read ticks, as on the phone: one dim ✓ while a note waits for the lead,
    /// ✓✓ in the accent color once the lead has seen it and when answered; on a lead's
    /// message, ✓✓ in the accent once the user has read it. A failed note says so
    /// in red. Hover for what it means.
    fn tick_marks(&self, m: &Msg, read: i64) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if m.from == "me" && m.status == "failed" {
            out.push(
                div()
                    .text_color(SeancePalette::danger())
                    .child("· not delivered")
                    .into_any_element(),
            );
        }
        if let Some((glyph, accent, tip_text)) = tick_for(m, read) {
            out.push(
                div()
                    .id(SharedString::from(format!("tick-{}", m.id)))
                    .font_semibold()
                    .text_color(if accent {
                        SeancePalette::violet()
                    } else {
                        SeancePalette::text_dim()
                    })
                    .tooltip(tip(tip_text))
                    .child(glyph)
                    .into_any_element(),
            );
        }
        out
    }

    /// Tap-to-answer pills in a wrapping row. The first is the lead's pick, so
    /// it is tinted. Once one is tapped (here or elsewhere) all go dead, the
    /// tapped one is filled and the rest are dimmed.
    fn choice_pills(&self, m: &Msg, cx: &Context<Self>) -> AnyElement {
        let picked = m
            .picked
            .clone()
            .or_else(|| self.chat.tapped.get(&m.id).cloned());
        let id = m.id;
        div()
            .max_w(relative(0.72))
            .flex()
            .flex_wrap()
            .gap_2()
            .children(m.choices.iter().enumerate().map(|(i, choice)| {
                let is_picked = picked.as_deref() == Some(choice.as_str());
                let lead_pick = i == 0 && picked.is_none();
                let pill = div()
                    .id(SharedString::from(format!("choice-{id}-{i}")))
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .text_sm()
                    .border_1()
                    .border_color(if is_picked || lead_pick {
                        SeancePalette::violet()
                    } else {
                        SeancePalette::border()
                    })
                    .text_color(if is_picked {
                        SeancePalette::bg()
                    } else if lead_pick {
                        SeancePalette::violet()
                    } else {
                        SeancePalette::text()
                    })
                    .when(is_picked, |d| d.bg(SeancePalette::violet()))
                    .when(lead_pick, |d| d.bg(SeancePalette::violet().opacity(0.14)))
                    .when(picked.is_some() && !is_picked, |d| d.opacity(0.4))
                    .child(choice.clone());
                if picked.is_some() {
                    return pill;
                }
                let spec_change = changes_spec(m, choice);
                let choice = choice.clone();
                pill.cursor_pointer()
                    .hover(|s| s.bg(SeancePalette::violet().opacity(0.25)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if spec_change {
                            this.chat.focus_input = true;
                            cx.notify();
                        } else {
                            this.chat_answer(id, choice.clone(), cx);
                        }
                    }))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_body_drops_its_title_line() {
        assert_eq!(spec_body("Title\nbody\nmore"), "body\nmore");
        assert_eq!(spec_body("Title\n\nbody"), "body");
        assert_eq!(spec_body("Title only"), "");
    }

    #[test]
    fn change_only_refocuses_on_a_spec_card() {
        let spec = Msg {
            spec: Some("T".into()),
            ..Default::default()
        };
        assert!(changes_spec(&spec, "Change"));
        assert!(!changes_spec(&spec, "Go"));
        assert!(!changes_spec(&Msg::default(), "Change"));
    }

    #[test]
    fn markdown_reads_back_as_the_same_text() {
        assert_eq!(to_markdown("a *b* _c_ #d"), "a \\*b\\* \\_c\\_ \\#d");
        assert_eq!(to_markdown("1. one\n\ntwo"), "1\\. one\n\n\u{a0}\n\ntwo");
    }

    #[test]
    fn every_newline_is_a_paragraph_of_its_own() {
        use markdown::mdast::Node;
        let md = to_markdown("png\nimage: /home/x.png\nimage: /home/y.png");
        let Node::Root(root) = markdown::to_mdast(&md, &markdown::ParseOptions::default()).unwrap()
        else {
            panic!("no root")
        };
        // Three lines, three paragraphs, and no break node the renderer would drop.
        assert_eq!(root.children.len(), 3);
        assert!(root
            .children
            .iter()
            .all(|n| matches!(n, Node::Paragraph(_))));
    }

    #[test]
    fn videos_are_told_apart_by_extension() {
        for v in ["a.mp4", "B.MOV", "x.webm", "y.m4v"] {
            assert!(is_video(v), "{v}");
        }
        for p in ["a.png", "b.jpeg", "mp4", "clip.mp4.png"] {
            assert!(!is_video(p), "{p}");
        }
    }

    #[test]
    fn urls_become_links_without_their_trailing_punctuation() {
        assert_eq!(
            to_markdown("see https://example.com/x/. ok"),
            "see [https\\:\\/\\/example\\.com\\/x\\/](<https://example.com/x/>)\\. ok"
        );
        assert!(to_markdown("(https://a.b/c)").contains("](<https://a.b/c>)"));
    }

    #[test]
    fn a_double_clicked_word_finds_only_its_own_path() {
        let text = "wrote `~/Claude/out/a.png`, and /tmp/b.txt. See https://x.y/z";
        assert_eq!(path_of_word(text, "out"), Some("~/Claude/out/a.png"));
        assert_eq!(path_of_word(text, "b.txt"), Some("/tmp/b.txt"));
        assert_eq!(path_of_word(text, "nope"), None);
        // A URL is not a file path.
        assert_eq!(path_of_word(text, "x.y"), None);
    }

    #[test]
    fn a_word_of_the_sentence_is_not_a_click_on_the_file() {
        let text = "I put the png in ~/Claude/out/a.png";
        assert_eq!(path_of_word(text, "png"), None);
        assert_eq!(path_of_word(text, "a.png"), Some("~/Claude/out/a.png"));
        // Punctuation picked by the word select is not a path word.
        assert_eq!(path_of_word(text, "/"), None);
        assert_eq!(path_of_word(text, "."), None);
        // The same word in two paths cannot say which was clicked.
        assert_eq!(path_of_word("/a/out/x and /b/out/y", "out"), None);
    }

    #[test]
    fn indentation_and_blank_space_survive_as_markdown() {
        let nb = "\u{a0}";
        assert_eq!(to_markdown("    code"), format!("{nb}{nb}{nb}{nb}code"));
        assert_eq!(to_markdown("\tx"), format!("{nb}{nb}{nb}{nb}x"));
        assert_eq!(to_markdown("a\n  - b"), format!("a\n\n{nb}{nb}\\- b"));
        // Whitespace-only lines are blank lines; the tail of a message is not.
        assert_eq!(to_markdown("a\n   \nb"), format!("a\n\n{nb}\n\nb"));
        assert_eq!(to_markdown("a\n   "), "a");
        assert_eq!(to_markdown("a\n\n"), "a");
    }

    #[test]
    fn reset_labels_use_a_12_hour_clock() {
        // Whatever the machine's zone, the shape is fixed.
        let short = reset_label(1_793_000_000_000, false);
        assert!(short.ends_with("AM") || short.ends_with("PM"), "{short}");
        assert!(short.contains(':'), "{short}");
        let long = reset_label(1_793_000_000_000, true);
        assert!(
            !long.contains(':') && long.split(' ').count() == 3,
            "{long}"
        );
    }

    #[test]
    fn usage_strip_hides_without_windows() {
        assert!(usage_strip(Some(&Usage::default()), None, None).is_none());
        assert!(usage_strip(None, None, None).is_none());
        let one = Usage {
            five_hour: Some(UsageWindow {
                used: 18.,
                resets: None,
            }),
            seven_day: None,
        };
        assert!(usage_strip(Some(&one), None, None).is_some());
        // The lead's context alone is enough to show the strip.
        assert!(usage_strip(None, Some(28), None).is_some());
        // Calm under 30, yellow from 30, orange from 50 (Compact's threshold).
        assert_eq!(
            [0, 29, 30, 49, 50, 100].map(ctx_heat),
            [
                Heat::Calm,
                Heat::Calm,
                Heat::Yellow,
                Heat::Yellow,
                Heat::Orange,
                Heat::Orange
            ]
        );
    }

    #[test]
    fn active_is_unread_or_recent_newest_first_and_never_alerts() {
        let now = 10 * ACTIVE_MS;
        let circle = |ws: &str, unread: u32, age: Option<u64>| CircleRow {
            ws: ws.into(),
            unread,
            last: age.map(|a| Msg {
                ts: now - a,
                ..Default::default()
            }),
            ..Default::default()
        };
        let circles = [
            circle("old", 0, Some(ACTIVE_MS + 1)),
            circle("recent", 0, Some(60_000)),
            circle("unread-old", 2, Some(5 * ACTIVE_MS)),
            circle("alerts", 3, Some(1_000)),
            circle("quiet", 0, None),
            circle("newest", 0, Some(1_000)),
            circle("unread-empty", 1, None),
        ];
        let got: Vec<String> = active_circles(&circles, now)
            .into_iter()
            .map(|c| c.ws)
            .collect();
        assert_eq!(got, ["newest", "recent", "unread-old", "unread-empty"]);
    }

    #[test]
    fn ticks_follow_a_note_s_status_and_the_read_mark() {
        let mine = |status: &str| Msg {
            id: 5,
            from: "me".into(),
            status: status.into(),
            ..Default::default()
        };
        assert_eq!(
            tick_for(&mine("sending"), 0),
            Some(("✓", false, "Waiting for the lead"))
        );
        assert_eq!(
            tick_for(&mine("got"), 0),
            Some(("✓✓", true, "The lead has seen it"))
        );
        assert_eq!(
            tick_for(&mine("answered"), 0),
            Some(("✓✓", true, "Answered"))
        );
        assert_eq!(tick_for(&mine("failed"), 0), None);
        let lead = |id| Msg {
            id,
            from: "lead".into(),
            ..Default::default()
        };
        assert_eq!(tick_for(&lead(9), 9), Some(("✓✓", true, "Seen")));
        assert_eq!(tick_for(&lead(10), 9), None);
        assert_eq!(tick_for(&lead(-1), 9), None);
    }

    #[test]
    fn a_refresh_rereads_the_newest_twenty_messages() {
        let mut state = ChatState::default();
        let msg = |id| Msg {
            id,
            ..Default::default()
        };
        // Short: the whole thread.
        state.threads.insert(
            "w".into(),
            Thread {
                messages: (1..=20).map(msg).collect(),
                ..Default::default()
            },
        );
        assert_eq!(state.refresh_after("w"), None);
        // Longer: from just before the 20th from the end; ids are not dense.
        state.threads.insert(
            "w".into(),
            Thread {
                messages: (1..=30).map(|i| msg(i * 3)).collect(),
                ..Default::default()
            },
        );
        assert_eq!(state.refresh_after("w"), Some(30));
        assert_eq!(state.refresh_after("none"), None);
    }

    #[test]
    fn pictures_fit_the_bubble_at_their_own_shape() {
        // A portrait phone photo is held by its height, a wide screenshot by its width.
        assert_eq!(fit_image(3000, 4000), (315., 420.));
        assert_eq!(fit_image(1600, 800), (320., 160.));
        // Small ones are not blown up; unknown sizes get the loading box.
        assert_eq!(fit_image(100, 50), (100., 50.));
        assert_eq!(fit_image(0, 0), (IMG_MAX_W, IMG_LOADING_H));
    }

    #[test]
    fn preview_is_one_line_and_marks_your_messages() {
        let m = Msg {
            from: "me".into(),
            text: "hello\n  there".into(),
            ..Default::default()
        };
        assert_eq!(preview(&m), "You: hello there");
        let photo = Msg {
            from: "lead".into(),
            image: Some("a.png".into()),
            ..Default::default()
        };
        assert_eq!(preview(&photo), "📷 photo");
    }

    #[test]
    fn reminders_land_on_the_hour_the_menu_says() {
        const HOUR: u64 = 3600 * 1000;
        let now = now_ms();
        let [hour, three, tomorrow, monday] = remind_times(now);
        assert_eq!((hour, three), (now + HOUR, now + 3 * HOUR));
        // Local 9:00, tomorrow (a day ahead by the calendar) and a Monday.
        assert_eq!(events::local_clock(tomorrow).1, 9);
        assert_eq!(events::local_clock(tomorrow).2, 0);
        assert!(tomorrow > now && tomorrow - now <= 48 * HOUR);
        assert_eq!(events::local_clock(monday).0, 1);
        assert_eq!(events::local_clock(monday).1, 9);
        assert!(monday > now && monday - now <= 8 * 24 * HOUR);
    }

    #[test]
    fn a_note_that_is_only_its_document_shows_only_the_link() {
        let doc = |text: &str| Msg {
            text: text.into(),
            attachment: Some(Attachment {
                file: "a.pdf".into(),
                title: "plan.pdf".into(),
            }),
            ..Default::default()
        };
        assert!(attachment_only(&doc("📎 plan.pdf")));
        assert!(!attachment_only(&doc("📎 plan.pdf, please read")));
        assert!(!attachment_only(&Msg {
            text: "📎 plan.pdf".into(),
            ..Default::default()
        }));
    }

    #[test]
    fn search_waits_for_two_characters() {
        assert_eq!(search_query("  "), Err(None));
        assert_eq!(search_query(" a "), Err(Some("Type at least 2 characters")));
        assert_eq!(search_query(" ab "), Ok("ab"));
        assert_eq!(search_query("é1"), Ok("é1"));
    }

    #[test]
    fn the_thread_follows_the_bottom_until_he_scrolls_up() {
        // (offset, range): pinned and the content grew, offset unchanged.
        assert_eq!(
            follow_step(true, false, Some((-900., 900.)), (-900., 1000.)),
            (true, true)
        );
        // He scrolled up by hand: range unchanged, offset moved well off the bottom.
        assert_eq!(
            follow_step(true, false, Some((-900., 900.)), (-500., 900.)),
            (false, false)
        );
        // Then new content arrives: unpinned, so nothing pulls him down.
        assert_eq!(
            follow_step(false, false, Some((-500., 900.)), (-500., 1000.)),
            (false, false)
        );
        // Back within 80 points of the bottom: pinned again.
        assert_eq!(
            follow_step(false, false, Some((-500., 900.)), (-850., 900.)),
            (true, false)
        );
        // A little scroll up stays pinned but is not undone.
        assert_eq!(
            follow_step(true, false, Some((-900., 900.)), (-880., 900.)),
            (true, false)
        );
        // Past the bottom (overscroll): pinned, nothing to scroll.
        assert_eq!(
            follow_step(true, false, Some((-900., 900.)), (-930., 900.)),
            (true, false)
        );
        // Clamped onto the bottom by a resize while unpinned: pinned again.
        assert_eq!(
            follow_step(false, false, Some((-500., 900.)), (-1000., 1000.)),
            (true, false)
        );
        // First frame: pinned and off the bottom, scroll down.
        assert_eq!(follow_step(true, false, None, (0., 400.)), (true, true));
        // A search result being shown: never scrolled, whatever grows.
        assert_eq!(
            follow_step(true, true, Some((-100., 900.)), (-100., 1000.)),
            (true, false)
        );
        // A hand scroll while a result is shown still tracks the pin.
        assert_eq!(
            follow_step(true, true, Some((-900., 900.)), (-300., 900.)),
            (false, false)
        );
        // At the bottom already: nothing to do.
        assert_eq!(
            follow_step(true, false, Some((-900., 900.)), (-900., 900.)),
            (true, false)
        );
    }
}
