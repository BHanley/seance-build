//! Pure rules behind the Wall: the Today feed from the daemon's event log,
//! a tile's state and text, the grid's columns and placement, and arrow-key
//! moves.
//! No gpui here, so every rule is unit-tested.

use std::collections::HashMap;

use crate::events::Event;

use super::stage_parse::{ellipsize, latest_finish, strip_markdown, Health, HealthLevel};
use super::views::ViewMode;

/// Rows the feed shows.
pub(super) const FEED_CAP: usize = 50;
/// Event kinds the feed is built from. `status_set` carries needs-you and
/// done, and the working stretch before a done gives its duration.
pub(super) const FEED_KINDS: [&str; 5] = [
    "status_set",
    "pane_exited",
    "lead_set",
    "workspace_slept",
    "workspace_woke",
];
/// Raw events kept between polls, and the most one poll asks for (the
/// daemon's own cap). A busy day is a few thousand.
pub(super) const RAW_CAP: usize = 5000;
/// Below this window width the feed folds behind a button.
pub(super) const FEED_FOLD_BELOW: f32 = 1100.;

/// A feed row's dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dot {
    Needs,
    Done,
    Down,
    Flame,
    Faint,
}

/// One line of the Today feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FeedRow {
    pub ts: u64,
    /// The circle's slug, when the event or an earlier one named it.
    pub circle: Option<String>,
    /// The pane it is about, when the row reads better with its name in
    /// front (the view adds it unless the pane leads its circle).
    pub pane: Option<String>,
    pub text: String,
    pub dot: Dot,
    /// Shown in place of a circle name (the health check has none).
    pub title: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    Working,
    Needs,
    Blocked,
    Risky,
    Done,
    Other,
}

/// Turn the day's events (oldest first) into feed rows, newest first, at
/// most `cap`. Statuses are re-set often, so a row marks a change: the
/// first needs-you of a stretch, a done, an exit. Done reads "done after
/// Nm" from the start of the working stretch before it.
pub(super) fn shape_feed(events: &[Event], day_start: u64, cap: usize) -> Vec<FeedRow> {
    let mut last: HashMap<&str, Seen> = HashMap::new();
    let mut working_since: HashMap<&str, u64> = HashMap::new();
    let mut circle_of: HashMap<&str, &str> = HashMap::new();
    let mut rows = Vec::new();
    for e in events {
        let pane = e.pane.as_deref().unwrap_or("");
        if let (Some(ws), false) = (e.workspace.as_deref(), pane.is_empty()) {
            circle_of.insert(pane, ws);
        }
        let circle = e
            .workspace
            .as_deref()
            .or_else(|| circle_of.get(pane).copied())
            .map(str::to_string);
        let row = |text: String, dot: Dot, pane: Option<&str>| FeedRow {
            ts: e.ts,
            circle: circle.clone(),
            pane: pane.map(str::to_string),
            text,
            dot,
            title: None,
        };
        let new_row = match e.kind.as_str() {
            "status_set" if !pane.is_empty() => {
                let (state, note) = match e.detail.split_once(':') {
                    Some((s, n)) => (s.trim(), Some(n.trim()).filter(|n| !n.is_empty())),
                    None => (e.detail.trim(), None),
                };
                let seen = match state {
                    "working" | "planning" => Seen::Working,
                    "needs-human" => Seen::Needs,
                    "blocked" => Seen::Blocked,
                    "risky" => Seen::Risky,
                    "done" => Seen::Done,
                    _ => Seen::Other,
                };
                let prev = last.insert(pane, seen);
                let changed = prev != Some(seen);
                match seen {
                    Seen::Working => {
                        working_since.entry(pane).or_insert(e.ts);
                        None
                    }
                    Seen::Needs | Seen::Blocked | Seen::Risky if changed => {
                        let text = note.map(str::to_string).unwrap_or_else(|| {
                            match seen {
                                Seen::Blocked => "is blocked",
                                Seen::Risky => "is about to do something risky",
                                _ => "needs you",
                            }
                            .to_string()
                        });
                        Some(row(text, Dot::Needs, Some(pane)))
                    }
                    Seen::Done => {
                        let since = working_since.remove(pane);
                        changed.then(|| {
                            let mut text = match since {
                                Some(t) => format!("done after {}", span_label(e.ts - t.min(e.ts))),
                                None => "done".to_string(),
                            };
                            if let Some(n) = note {
                                text.push_str(" · ");
                                text.push_str(n);
                            }
                            row(text, Dot::Done, Some(pane))
                        })
                    }
                    Seen::Other => {
                        // idle / turn ended: the working stretch is over.
                        working_since.remove(pane);
                        None
                    }
                    _ => None,
                }
            }
            "pane_exited" if !pane.is_empty() => {
                last.remove(pane);
                working_since.remove(pane);
                let text = match exit_code(&e.detail) {
                    Some(c) if c != 0 => format!("{pane} exited with code {c}"),
                    _ => format!("{pane} exited"),
                };
                Some(row(text, Dot::Down, None))
            }
            "lead_set" => {
                let who = if e.detail.is_empty() { pane } else { &e.detail };
                Some(row(format!("{who} took the lead"), Dot::Flame, None))
            }
            "workspace_slept" => Some(row("put to sleep".into(), Dot::Faint, None)),
            "workspace_woke" => Some(row("woke".into(), Dot::Faint, None)),
            _ => None,
        };
        if let Some(mut r) = new_row.filter(|_| e.ts >= day_start) {
            r.text = ellipsize(&r.text, 90);
            rows.push(r);
        }
    }
    rows.reverse();
    rows.truncate(cap);
    rows
}

/// The code in a `pane_exited` detail: "process exited (Some(1)) ..." → 1.
fn exit_code(detail: &str) -> Option<i32> {
    let start = detail.find("Some(")? + "Some(".len();
    let end = detail[start..].find(')')?;
    detail[start..start + end].trim().parse().ok()
}

/// "45s", "6m", "1h 20m".
pub(super) fn span_label(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ if (s / 60).is_multiple_of(60) => format!("{}h", s / 3600),
        _ => format!("{}h {}m", s / 3600, (s / 60) % 60),
    }
}

/// The health check as a feed row, when its report is from today.
pub(super) fn health_row(h: &Health, today: &str, day_start: u64) -> Option<FeedRow> {
    if h.day.as_deref() != Some(today) {
        return None;
    }
    let (hh, mm) = h.at.as_deref()?.split_once(':')?;
    let mins: u64 = hh.parse::<u64>().ok()? * 60 + mm.parse::<u64>().ok()?;
    let mut text = format!("{} {}", h.host_label(), h.status);
    if let Some(l) = &h.leads {
        text.push_str(" · ");
        text.push_str(l);
    }
    Some(FeedRow {
        ts: day_start + mins * 60_000,
        circle: None,
        pane: None,
        text,
        dot: match h.level {
            HealthLevel::Critical => Dot::Down,
            _ => Dot::Flame,
        },
        title: Some("Health check"),
    })
}

/// Daemon rows plus rows this window made (health, the Mac), newest first.
pub(super) fn merge_rows(mut rows: Vec<FeedRow>, extra: &[FeedRow], cap: usize) -> Vec<FeedRow> {
    rows.extend(extra.iter().cloned());
    // Stable: equal times keep the daemon's order.
    rows.sort_by_key(|r| std::cmp::Reverse(r.ts));
    rows.truncate(cap);
    rows
}

/// What a circle's tile shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TileState {
    Needs,
    LeadExited,
    MacAsleep,
    Working,
    Done,
    Idle,
    /// Put to sleep: folded into the "+N asleep" tile.
    Asleep,
}

/// The facts a tile's state is read from.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TileFacts {
    pub asleep: bool,
    pub needs: bool,
    pub lead_exited: bool,
    pub mac_waiting: bool,
    pub working: bool,
    pub done: bool,
}

/// The most pressing thing first: a sleeping circle is folded away, then
/// anything waiting on you, then a lead that is gone or out of reach.
pub(super) fn tile_state(f: TileFacts) -> TileState {
    if f.asleep {
        TileState::Asleep
    } else if f.needs {
        TileState::Needs
    } else if f.lead_exited {
        TileState::LeadExited
    } else if f.mac_waiting {
        TileState::MacAsleep
    } else if f.working {
        TileState::Working
    } else if f.done {
        TileState::Done
    } else {
        TileState::Idle
    }
}

/// `claude-mac` prints this while the Mac sleeps and it waits to resume.
pub(super) fn mac_waiting(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        l.starts_with("Mac is asleep or out of reach") || l == "(Ctrl-C closes the pane instead.)"
    })
}

/// A pane title as a task name, glyph stripped. Trusted only while it
/// looks live: Claude's ✳ or spinner in front, or the title of a program
/// that isn't a shell or Claude. A Claude that fell back to a shell can
/// leave its last task behind, so a plain title there is hidden, as are
/// empty titles, Claude's own name and a shell's `user@host:dir`.
pub(super) fn task_title(title: &str, command: &str) -> Option<String> {
    let live = title.trim_start().starts_with('✳') || seance_core::util::title_looks_busy(title);
    let prog = command
        .split_whitespace()
        .filter(|t| !t.contains('='))
        .map(|t| t.rsplit('/').next().unwrap_or(t))
        .find(|t| *t != "env")
        .unwrap_or("");
    let shell = |p: &str| matches!(p, "" | "bash" | "zsh" | "sh" | "fish" | "nu");
    if !live && (shell(prog) || prog.starts_with("claude")) {
        return None;
    }
    let t = title
        .trim_start_matches(|c: char| c.is_whitespace() || !(c.is_ascii() || c.is_alphanumeric()))
        .trim();
    let plain = t.eq_ignore_ascii_case("claude code")
        || t == prog
        || shell(t)
        || (t.contains('@') && t.contains(':') && !t.contains(' '));
    (!plain).then(|| t.to_string())
}

/// Claude's TUI chrome: spinner and timing lines, notices, the bare
/// prompt, box rules and the input box, and the status line. None of it
/// says what the pane is doing.
pub(super) fn is_claude_chrome(line: &str) -> bool {
    let t = line.trim();
    let lower = t.to_lowercase();
    let glyph = |c: char| !(c.is_ascii() || c.is_alphanumeric());
    let boxed = |c: char| ('\u{2500}'..='\u{257F}').contains(&c);
    let rest = t.trim_start_matches(|c: char| glyph(c) || c.is_whitespace());
    let mut words = rest.split_whitespace();
    // "✻ Churned for 59s · done 10:54 AM"
    let spinner = rest.len() < t.len()
        && words.next().is_some()
        && words.next() == Some("for")
        && words
            .next()
            .is_some_and(|d| d.starts_with(|c: char| c.is_ascii_digit()));
    // A rule, or the input box ("│ > │"); a table row has text in its cells.
    let inner: String = t.chars().filter(|c| !boxed(*c)).collect();
    let inner = inner.trim();
    let box_line = t.starts_with(boxed) && (inner.is_empty() || inner.starts_with(['>', '❯', '›']));
    t.is_empty()
        || spinner
        || box_line
        || matches!(t, ">" | "›" | "❯")
        || [
            "update available",
            "/clear to save",
            "message hidden",
            "messages hidden",
            "auto mode",
            "shift+tab",
            "esc to interrupt",
            "? for shortcuts",
        ]
        .iter()
        .any(|p| lower.contains(p))
        || ((lower.starts_with("ran ") || lower.contains(" ran "))
            && lower.contains("shell command"))
}

/// The newest screen line that is not chrome, for a tile with nothing else.
pub(super) fn last_real_line(lines: &[String]) -> Option<String> {
    lines.iter().rev().find(|l| !is_claude_chrome(l)).cloned()
}

/// The first `n` non-empty lines of a pad's newest finish note, as plain
/// text: heading and list marks, rules and emphasis dropped.
pub(super) fn finish_excerpt(pad: &str, n: usize) -> Vec<String> {
    let unmark = |l: &str| {
        let l = l.trim();
        let h = l.trim_start_matches('#');
        let l = if h.len() < l.len() && h.starts_with(' ') {
            h.trim_start()
        } else {
            l
        };
        ["- ", "* ", "> "]
            .iter()
            .find_map(|m| l.strip_prefix(m))
            .unwrap_or(l)
            .to_string()
    };
    latest_finish(pad, 64)
        .unwrap_or_default()
        .iter()
        .map(|l| strip_markdown(&unmark(l)))
        .filter(|l| !l.is_empty() && l != "…" && !l.chars().all(|c| matches!(c, '-' | '_')))
        .take(n)
        .map(|l| ellipsize(&l, 160))
        .collect()
}

/// A result as old as its pad: gone after a day, and "3h ago: " in front
/// once it is an hour old. A pad with no mtime counts as fresh.
pub(super) fn aged_result(lines: &[String], mtime: Option<u64>, now: u64) -> Vec<String> {
    const HOUR: u64 = 3_600_000;
    let age = mtime.map_or(0, |m| now.saturating_sub(m));
    let mut out = lines.to_vec();
    if age >= 24 * HOUR {
        out.clear();
    } else if age >= HOUR {
        if let Some(first) = out.first_mut() {
            *first = format!("{}h ago: {first}", age / HOUR);
        }
    }
    out
}

/// Result lines a small tile has room for: a short row fits one, or none
/// beside a task and a Doing line.
pub(super) fn result_rows(row_h: f32, title: bool, doing: bool, most: usize) -> usize {
    if row_h >= 180. {
        most
    } else if title && doing {
        0
    } else {
        most.min(1)
    }
}

/// Keys stop at the window on the Wall, after the app chords, unless a
/// field drawn over it (rename, the quicklaunch editor) is open.
pub(super) fn wall_takes_keys(mode: ViewMode, field_open: bool) -> bool {
    mode == ViewMode::Wall && !field_open
}

/// Whether app paths that write into the active pane (the prompts
/// palette) may run: not on the Wall, where the active pane is hidden.
pub(super) fn may_write_to_active(mode: ViewMode) -> bool {
    !matches!(mode, ViewMode::Wall | ViewMode::Chat)
}

/// Grid columns for the tile area's width: 4, dropping to 3 and 2.
pub(super) fn grid_cols(width: f32) -> usize {
    if width >= 900. {
        4
    } else if width >= 640. {
        3
    } else {
        2
    }
}

/// A tile's place in the grid, 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cell {
    pub row: usize,
    pub col: usize,
    /// 2 for the big tile, else 1.
    pub span: usize,
}

/// Place `n` tiles in reading order, the `big` one 2x2 at the first spot
/// it fits, the rest filling the first free cells (CSS `grid-auto-flow:
/// dense`), so the big tile sits near its rail position with no holes.
pub(super) fn place_tiles(n: usize, big: Option<usize>, cols: usize) -> Vec<Cell> {
    let cols = cols.max(1);
    let mut taken: Vec<Vec<bool>> = Vec::new();
    let free = |taken: &Vec<Vec<bool>>, r: usize, c: usize| {
        taken.get(r).is_none_or(|row: &Vec<bool>| !row[c])
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let span = if big == Some(i) && cols >= 2 { 2 } else { 1 };
        let mut r = 0;
        let spot = loop {
            if let Some(c) = (0..=cols - span)
                .find(|&c| (0..span).all(|dr| (0..span).all(|dc| free(&taken, r + dr, c + dc))))
            {
                break (r, c);
            }
            r += 1;
        };
        while taken.len() < spot.0 + span {
            taken.push(vec![false; cols]);
        }
        for dr in 0..span {
            for dc in 0..span {
                taken[spot.0 + dr][spot.1 + dc] = true;
            }
        }
        out.push(Cell {
            row: spot.0,
            col: spot.1,
            span,
        });
    }
    out
}

/// Rows the placed tiles use.
pub(super) fn rows_used(cells: &[Cell]) -> usize {
    cells.iter().map(|c| c.row + c.span).max().unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The tile an arrow key lands on: one wholly on that side, those sharing a
/// row (or column) with this tile first, then the nearest. None at an edge.
pub(super) fn move_selection(cells: &[Cell], from: usize, dir: Dir) -> Option<usize> {
    let a = cells.get(from)?;
    // Doubled coordinates keep a 2x2 tile's center on the integer grid.
    let center = |c: &Cell| ((c.row * 2 + c.span) as i64, (c.col * 2 + c.span) as i64);
    let (ar, ac) = center(a);
    cells
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != from)
        .filter(|(_, b)| match dir {
            Dir::Left => b.col + b.span <= a.col,
            Dir::Right => b.col >= a.col + a.span,
            Dir::Up => b.row + b.span <= a.row,
            Dir::Down => b.row >= a.row + a.span,
        })
        .min_by_key(|(i, b)| {
            let (br, bc) = center(b);
            let overlap = |a0: usize, a1: usize, b0: usize, b1: usize| a0 < b1 && b0 < a1;
            let (in_line, along, across) = match dir {
                Dir::Left | Dir::Right => (
                    overlap(a.row, a.row + a.span, b.row, b.row + b.span),
                    (bc - ac).abs(),
                    (br - ar).abs(),
                ),
                Dir::Up | Dir::Down => (
                    overlap(a.col, a.col + a.span, b.col, b.col + b.span),
                    (br - ar).abs(),
                    (bc - ac).abs(),
                ),
            };
            (!in_line, along, across, *i)
        })
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, ts: u64, ws: Option<&str>, pane: &str, kind: &str, detail: &str) -> Event {
        Event {
            id: format!("evt_{seq}"),
            seq,
            ts,
            actor: "daemon".into(),
            workspace: ws.map(str::to_string),
            pane: Some(pane.to_string()),
            kind: kind.into(),
            detail: detail.into(),
            caused_by: None,
            span: None,
            origin: None,
        }
    }

    const MIN: u64 = 60_000;

    #[test]
    fn feed_rows_mark_changes_newest_first() {
        let w = Some("follows");
        let events = vec![
            ev(1, 10 * MIN, w, "lead", "status_set", "working"),
            ev(2, 11 * MIN, w, "lead", "status_set", "working"),
            ev(
                3,
                12 * MIN,
                w,
                "lead",
                "status_set",
                "needs-human: wants to run git push",
            ),
            // A repeat of the same ask is not a new row.
            ev(
                4,
                13 * MIN,
                w,
                "lead",
                "status_set",
                "needs-human: wants to run git push",
            ),
            ev(5, 14 * MIN, w, "lead", "status_set", "working"),
            ev(
                6,
                16 * MIN,
                w,
                "lead",
                "status_set",
                "done: relaunch Seance",
            ),
            ev(7, 17 * MIN, w, "lead", "status_set", "done"),
            ev(
                8,
                18 * MIN,
                None,
                "lead",
                "pane_exited",
                "process exited (Some(1)) \u{2014} auto-closed",
            ),
            ev(9, 19 * MIN, w, "lead-2", "lead_set", "lead-2"),
            ev(10, 20 * MIN, w, "x", "terminal.input", "k"),
        ];
        let rows = shape_feed(&events, 0, FEED_CAP);
        let texts: Vec<(&str, Dot)> = rows.iter().map(|r| (r.text.as_str(), r.dot)).collect();
        assert_eq!(
            texts,
            [
                ("lead-2 took the lead", Dot::Flame),
                ("lead exited with code 1", Dot::Down),
                ("done after 6m · relaunch Seance", Dot::Done),
                ("wants to run git push", Dot::Needs),
            ]
        );
        // The exit names no circle; an earlier event about the pane did.
        assert_eq!(rows[1].circle.as_deref(), Some("follows"));
        assert_eq!(rows[1].pane, None);
        assert_eq!(rows[2].pane.as_deref(), Some("lead"));
        assert_eq!(rows[0].ts, 19 * MIN);
    }

    #[test]
    fn feed_is_today_only_and_capped() {
        let day = 1_000 * MIN;
        let mut events = vec![
            // Yesterday's needs-you is history, but it still counts as the
            // state this morning's repeat continues.
            ev(1, day - MIN, Some("a"), "p", "status_set", "needs-human"),
            ev(2, day + MIN, Some("a"), "p", "status_set", "needs-human"),
            ev(3, day + 2 * MIN, Some("a"), "q", "status_set", "blocked"),
        ];
        let rows = shape_feed(&events, day, FEED_CAP);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "is blocked");

        for i in 0..80u64 {
            events.push(ev(
                10 + i,
                day + (10 + i) * MIN,
                Some("a"),
                "p",
                "pane_exited",
                "x",
            ));
        }
        let rows = shape_feed(&events, day, FEED_CAP);
        assert_eq!(rows.len(), FEED_CAP);
        assert!(rows.windows(2).all(|w| w[0].ts >= w[1].ts));
        assert_eq!(rows[0].ts, day + 89 * MIN);
    }

    #[test]
    fn done_without_a_working_stretch_and_idle_resets() {
        let events = vec![
            ev(1, MIN, Some("a"), "p", "status_set", "working"),
            ev(2, 2 * MIN, Some("a"), "p", "status_set", "idle: turn ended"),
            ev(3, 3 * MIN, Some("a"), "p", "status_set", "done"),
            ev(
                4,
                4 * MIN,
                Some("a"),
                "p",
                "workspace_slept",
                "3 pane(s) slept",
            ),
        ];
        let rows = shape_feed(&events, 0, FEED_CAP);
        assert_eq!(rows[0].text, "put to sleep");
        assert_eq!(rows[0].dot, Dot::Faint);
        assert_eq!(rows[1].text, "done");
    }

    #[test]
    fn span_labels() {
        assert_eq!(span_label(45_000), "45s");
        assert_eq!(span_label(6 * MIN + 5_000), "6m");
        assert_eq!(span_label(120 * MIN), "2h");
        assert_eq!(span_label(80 * MIN), "1h 20m");
    }

    fn health(day: &str, at: &str, level: HealthLevel) -> Health {
        Health {
            level,
            status: "healthy".into(),
            at: Some(at.into()),
            day: Some(day.into()),
            host: Some("Devbox".into()),
            host_memory: None,
            mac: None,
            leads: Some("every circle has a lead".into()),
            plan: None,
        }
    }

    #[test]
    fn health_report_rows_only_today_and_merge_in_time_order() {
        let day = 500 * MIN;
        let h = health("2026-10-19", "13:02", HealthLevel::Healthy);
        let row = health_row(&h, "2026-10-19", day).unwrap();
        assert_eq!(row.ts, day + (13 * 60 + 2) * MIN);
        assert_eq!(row.text, "Devbox healthy · every circle has a lead");
        assert_eq!(row.dot, Dot::Flame);
        assert_eq!(health_row(&h, "2026-10-20", day), None);
        let crit = health("2026-10-19", "01:00", HealthLevel::Critical);
        assert_eq!(health_row(&crit, "2026-10-19", day).unwrap().dot, Dot::Down);

        let daemon = shape_feed(
            &[
                ev(1, day + 12 * 60 * MIN, Some("a"), "p", "status_set", "done"),
                ev(2, day + 14 * 60 * MIN, Some("a"), "p", "pane_exited", "x"),
            ],
            day,
            FEED_CAP,
        );
        let merged = merge_rows(daemon, &[row], 2);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].dot, Dot::Down);
        assert_eq!(merged[1].dot, Dot::Flame);
    }

    #[test]
    fn tile_state_puts_the_most_pressing_first() {
        let f = TileFacts::default();
        assert_eq!(tile_state(f), TileState::Idle);
        let all = TileFacts {
            asleep: false,
            needs: true,
            lead_exited: true,
            mac_waiting: true,
            working: true,
            done: true,
        };
        assert_eq!(tile_state(all), TileState::Needs);
        assert_eq!(
            tile_state(TileFacts {
                needs: false,
                ..all
            }),
            TileState::LeadExited
        );
        assert_eq!(
            tile_state(TileFacts {
                needs: false,
                lead_exited: false,
                ..all
            }),
            TileState::MacAsleep
        );
        assert_eq!(
            tile_state(TileFacts {
                working: true,
                done: true,
                ..f
            }),
            TileState::Working
        );
        assert_eq!(tile_state(TileFacts { done: true, ..f }), TileState::Done);
        assert_eq!(
            tile_state(TileFacts {
                asleep: true,
                ..all
            }),
            TileState::Asleep
        );
    }

    #[test]
    fn mac_waiting_reads_claude_macs_message() {
        let l = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(mac_waiting(&l(&[
            "Mac is asleep or out of reach. This pane stays open and",
            "(Ctrl-C closes the pane instead.)"
        ])));
        assert!(mac_waiting(&l(&["", "(Ctrl-C closes the pane instead.)"])));
        assert!(!mac_waiting(&l(&[
            "Mac is back. Resuming the conversation."
        ])));
        assert!(!mac_waiting(&[]));
    }

    #[test]
    fn task_titles_drop_the_glyph_and_hide_stale_or_plain_names() {
        let t = |title: &str| task_title(title, "claude");
        assert_eq!(
            t("✳ Seance Mac app build from ann").as_deref(),
            Some("Seance Mac app build from ann")
        );
        assert_eq!(t("⠂ Fix the tiles").as_deref(), Some("Fix the tiles"));
        assert_eq!(t("✳ Claude Code"), None);
        assert_eq!(t(""), None);
        assert_eq!(t("✳ "), None);
        // Claude gone, its task left behind or a shell's title: not live.
        assert_eq!(t("Fix the tiles"), None);
        assert_eq!(task_title("Fix the tiles", "claude-mac"), None);
        assert_eq!(task_title("Fix the tiles", "bash -l"), None);
        assert_eq!(task_title("demo@devbox:~/x", "bash -l"), None);
        assert_eq!(task_title("zsh", "/usr/bin/env claude-mac"), None);
        assert_eq!(
            task_title("✳ Review", "/usr/bin/env A=1 claude").as_deref(),
            Some("Review")
        );
        // Another program's title is its own.
        assert_eq!(
            task_title("htop - load", "htop").as_deref(),
            Some("htop - load")
        );
        assert_eq!(task_title("htop", "htop"), None);
        assert_eq!(
            task_title("vim notes.md", "/usr/bin/env vim").as_deref(),
            Some("vim notes.md")
        );
    }

    #[test]
    fn claude_chrome_is_filtered_and_real_lines_kept() {
        for l in [
            "✻ Churned for 59s · done 10:54 AM",
            "✻ Cooked for 48s",
            "  Update available! Run: mise upgrade",
            "new task? /clear to save 136.1k tokens",
            "⎿  Ran 1 shell command",
            "Ran 3 shell commands",
            "message hidden (/focus to show)",
            "2 messages hidden (/focus to show)",
            "❯",
            "❯   ",
            ">",
            "›",
            "────────────────",
            "╭──────╮",
            "│ > │",
            "│ > half typed │",
            "⏵⏵ auto mode on (shift+tab to cycle)",
            "· Thinking… (12s · esc to interrupt)",
            "  ? for shortcuts",
            "   ",
        ] {
            assert!(is_claude_chrome(l), "{l}");
        }
        for l in [
            "● Read src/app/sidebar.rs",
            "Three ideas, ranked",
            "Waiting for the build for 2 more minutes",
            "$ cargo test",
            "> quoted text",
            "❯ cargo test",
            "│ tiles │ 3 │",
        ] {
            assert!(!is_claude_chrome(l), "{l}");
        }
        let lines = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            last_real_line(&lines(&["Built it.", "✻ Cooked for 4s", "❯"])).as_deref(),
            Some("Built it.")
        );
        assert_eq!(last_real_line(&lines(&["❯", "Ran 1 shell command"])), None);
        assert_eq!(last_real_line(&[]), None);
    }

    #[test]
    fn finish_excerpt_is_the_first_plain_lines() {
        let pad = "notes\n---\n<!-- lead · 10:00 · finish -->\n\n## Shipped\n\n\
- **Tiles** show the task\n---\n* tests `pass`\n\n- see [PR](https://x)\n- fifth\n";
        assert_eq!(
            finish_excerpt(pad, 4),
            ["Shipped", "Tiles show the task", "tests pass", "see PR"]
        );
        assert_eq!(finish_excerpt(pad, 1), ["Shipped"]);
        // A minus sign or a hashtag is text, not a mark.
        let pad = "<!-- a · 1 · finish -->\n-5% memory\n- -3 panes\n#42 fixed\n";
        assert_eq!(
            finish_excerpt(pad, 4),
            ["-5% memory", "-3 panes", "#42 fixed"]
        );
        assert!(finish_excerpt("no finish here", 4).is_empty());
        assert!(finish_excerpt("", 4).is_empty());
    }

    #[test]
    fn results_age_out_and_say_how_old() {
        let lines = vec!["Shipped".to_string(), "tests pass".to_string()];
        let h = 60 * MIN;
        let now = 100 * h;
        assert_eq!(aged_result(&lines, Some(now - 5 * MIN), now), lines);
        assert_eq!(aged_result(&lines, None, now), lines);
        assert_eq!(
            aged_result(&lines, Some(now - 3 * h - MIN), now),
            ["3h ago: Shipped", "tests pass"]
        );
        assert!(aged_result(&lines, Some(now - 24 * h), now).is_empty());
        // A clock behind the pad's is not an age.
        assert_eq!(aged_result(&lines, Some(now + h), now), lines);
    }

    #[test]
    fn short_rows_show_fewer_result_lines() {
        assert_eq!(result_rows(260., true, true, 3), 3);
        assert_eq!(result_rows(180., true, true, 3), 3);
        assert_eq!(result_rows(140., true, true, 3), 0);
        assert_eq!(result_rows(140., true, false, 3), 1);
        assert_eq!(result_rows(140., false, false, 3), 1);
    }

    #[test]
    fn only_the_wall_without_a_field_open_takes_keys() {
        assert!(wall_takes_keys(ViewMode::Wall, false));
        assert!(!wall_takes_keys(ViewMode::Wall, true));
        assert!(!wall_takes_keys(ViewMode::Classic, false));
        assert!(!wall_takes_keys(ViewMode::Stage, false));
    }

    #[test]
    fn nothing_writes_to_the_hidden_active_pane_on_the_wall() {
        assert!(!may_write_to_active(ViewMode::Wall));
        assert!(may_write_to_active(ViewMode::Classic));
        assert!(may_write_to_active(ViewMode::Stage));
    }

    #[test]
    fn columns_drop_with_width() {
        assert_eq!(grid_cols(1280. - 286.), 4);
        assert_eq!(grid_cols(900.), 4);
        assert_eq!(grid_cols(899.), 3);
        assert_eq!(grid_cols(640.), 3);
        assert_eq!(grid_cols(500.), 2);
        assert_eq!(grid_cols(0.), 2);
    }

    fn cell(row: usize, col: usize, span: usize) -> Cell {
        Cell { row, col, span }
    }

    #[test]
    fn big_tile_takes_2x2_where_it_fits_and_the_rest_fill_in() {
        // Selected first, as in the mockup: big top-left, 2 beside it on
        // each row, the rest below.
        let cells = place_tiles(9, Some(0), 4);
        assert_eq!(
            cells,
            [
                cell(0, 0, 2),
                cell(0, 2, 1),
                cell(0, 3, 1),
                cell(1, 2, 1),
                cell(1, 3, 1),
                cell(2, 0, 1),
                cell(2, 1, 1),
                cell(2, 2, 1),
                cell(2, 3, 1),
            ]
        );
        assert_eq!(rows_used(&cells), 3);
        // Fourth in a 4-wide row has no room beside it: it drops a row and
        // the next tile fills the gap it left.
        let cells = place_tiles(6, Some(3), 4);
        assert_eq!(cells[3], cell(1, 0, 2));
        assert_eq!(cells[4], cell(0, 3, 1));
        assert_eq!(cells[5], cell(1, 2, 1));
        // No selection, or one column: all 1x1 in reading order.
        assert_eq!(
            place_tiles(3, None, 2),
            [cell(0, 0, 1), cell(0, 1, 1), cell(1, 0, 1)]
        );
        assert_eq!(place_tiles(2, Some(1), 1), [cell(0, 0, 1), cell(1, 0, 1)]);
        assert!(place_tiles(0, Some(0), 4).is_empty());
        assert_eq!(rows_used(&[]), 0);
    }

    #[test]
    fn arrows_move_to_the_nearest_tile_in_line() {
        let cells = place_tiles(9, Some(0), 4);
        // From the big tile: right is the tile beside its top row, down
        // the first one under it.
        assert_eq!(move_selection(&cells, 0, Dir::Right), Some(1));
        assert_eq!(move_selection(&cells, 0, Dir::Down), Some(5));
        assert_eq!(move_selection(&cells, 0, Dir::Left), None);
        assert_eq!(move_selection(&cells, 0, Dir::Up), None);
        // Back into the big tile from either side, even when a tile in
        // the next row is nearer by distance alone.
        assert_eq!(move_selection(&cells, 3, Dir::Left), Some(0));
        assert_eq!(move_selection(&cells, 6, Dir::Up), Some(0));
        assert_eq!(move_selection(&cells, 2, Dir::Down), Some(4));
        assert_eq!(move_selection(&cells, 8, Dir::Right), None);
        assert_eq!(move_selection(&cells, 99, Dir::Right), None);
    }
}
