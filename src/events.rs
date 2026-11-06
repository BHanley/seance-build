//! The event bus: seance's flight recorder and pub/sub spine.
//!
//! Every meaningful action — human UI actions and agent control-plane calls
//! alike — becomes a typed, sequenced, attributable event. JSONL at
//! `~/.local/share/seance/events.jsonl` is the durable subscriber; in-process
//! watchers (and `seance ctl watch`) receive live copies.
//!
//! Actors: `"human"` (UI), `"agent:<pane-slug>"` (ctl from a pane), `"cli"`
//! (ctl outside seance), `"daemon"`, `"system"`.
//!
//! Optional fields (`caused_by`, `span`, `origin`) enable causal chains and
//! byte-level / action-level attribution without breaking old log lines.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How many recent events stay in the in-memory ring for catch-up on subscribe.
const RING_CAP: usize = 4096;

/// One attributed event. Old log lines without `id`/`seq` still deserialize.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Stable id (`evt_<seq>`). Empty on legacy lines.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// Monotonic sequence (process-global, restarts at 1 after daemon reboot
    /// unless recovered from the log tail — see [`Bus::load_seq_from_disk`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub seq: u64,
    /// Unix millis.
    pub ts: u64,
    /// "human" | "agent:<slug>" | "cli" | "daemon" | "system"
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The pane acted UPON (not the actor's own pane).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// Short machine-readable kind (see module docs / roadmap).
    pub kind: String,
    /// Human-readable one-liner.
    pub detail: String,
    /// Causal parent event id, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<String>,
    /// Span id grouping related events (e.g. a command run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
    /// Provenance of an input/mutation: human_keystroke | ctl_send |
    /// ctl_send_raw | propose_accepted | inject | arm | shell_hook | …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Optional fields for [`log_ex`].
#[derive(Default, Clone, Debug)]
pub struct LogOpts {
    pub caused_by: Option<String>,
    pub span: Option<String>,
    pub origin: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn log_path() -> PathBuf {
    if let Ok(dir) = std::env::var("SEANCE_STATE_DIR") {
        if !dir.is_empty() {
            let expanded = shellexpand::full(&dir)
                .map(|s| s.into_owned())
                .unwrap_or(dir);
            return PathBuf::from(expanded).join("events.jsonl");
        }
    }
    PathBuf::from(shellexpand::tilde("~/.local/share/seance").into_owned()).join("events.jsonl")
}

// ---------------------------------------------------------------------------
// In-process bus
// ---------------------------------------------------------------------------

struct Bus {
    seq: AtomicU64,
    ring: Mutex<VecDeque<Event>>,
    subs: Mutex<Vec<Sender<Event>>>,
}

impl Bus {
    fn new() -> Self {
        let bus = Self {
            seq: AtomicU64::new(0),
            ring: Mutex::new(VecDeque::with_capacity(RING_CAP)),
            subs: Mutex::new(Vec::new()),
        };
        bus.load_seq_from_disk();
        bus
    }

    /// Best-effort: start seq after the highest seq already on disk so
    /// restarts don't reuse ids. Legacy lines without seq are ignored.
    fn load_seq_from_disk(&self) {
        let Ok(content) = std::fs::read_to_string(log_path()) else {
            return;
        };
        let mut max = 0u64;
        for line in content.lines().rev().take(500) {
            if let Ok(e) = serde_json::from_str::<Event>(line) {
                if e.seq > max {
                    max = e.seq;
                }
            }
        }
        if max > 0 {
            self.seq.store(max, Ordering::SeqCst);
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn publish(&self, mut event: Event) -> Event {
        if event.seq == 0 {
            event.seq = self.next_seq();
        }
        if event.id.is_empty() {
            event.id = format!("evt_{}", event.seq);
        }
        if event.ts == 0 {
            event.ts = now_ms();
        }

        // Durable append first (best-effort).
        let path = log_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string(&event) {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "{json}");
            }
        }

        // Ring.
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= RING_CAP {
                ring.pop_front();
            }
            ring.push_back(event.clone());
        }

        // Live subscribers (drop dead ones).
        if let Ok(mut subs) = self.subs.lock() {
            subs.retain(|tx| tx.send(event.clone()).is_ok());
        }

        event
    }

    fn subscribe(&self) -> (Receiver<Event>, u64) {
        let (tx, rx) = mpsc::channel();
        let cursor = self.seq.load(Ordering::SeqCst);
        if let Ok(mut subs) = self.subs.lock() {
            subs.push(tx);
        }
        (rx, cursor)
    }

    fn ring_since(&self, since_seq: u64) -> Vec<Event> {
        let Ok(ring) = self.ring.lock() else {
            return Vec::new();
        };
        ring.iter().filter(|e| e.seq > since_seq).cloned().collect()
    }
}

fn bus() -> &'static Bus {
    static BUS: OnceLock<Bus> = OnceLock::new();
    BUS.get_or_init(Bus::new)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Append + publish one event. Best-effort: failure never breaks the app.
/// Returns the fully stamped event (with id/seq).
pub fn log(
    actor: &str,
    workspace: Option<&str>,
    pane: Option<&str>,
    kind: &str,
    detail: String,
) -> Event {
    log_ex(actor, workspace, pane, kind, detail, LogOpts::default())
}

/// Like [`log`] with causal / origin metadata.
pub fn log_ex(
    actor: &str,
    workspace: Option<&str>,
    pane: Option<&str>,
    kind: &str,
    detail: String,
    opts: LogOpts,
) -> Event {
    let event = Event {
        id: String::new(),
        seq: 0,
        ts: 0,
        actor: actor.to_string(),
        workspace: workspace.map(str::to_string),
        pane: pane.map(str::to_string),
        kind: kind.to_string(),
        detail,
        caused_by: opts.caused_by,
        span: opts.span,
        origin: opts.origin,
    };
    bus().publish(event)
}

/// Subscribe to live events. Returns `(receiver, current_seq)`.
/// Use `ring_since` / `read` for catch-up, then drain the receiver.
pub fn subscribe() -> (Receiver<Event>, u64) {
    bus().subscribe()
}

/// In-memory ring events with `seq > since_seq`.
pub fn ring_since(since_seq: u64) -> Vec<Event> {
    bus().ring_since(since_seq)
}

/// Current sequence high-water mark.
pub fn current_seq() -> u64 {
    bus().seq.load(Ordering::SeqCst)
}

/// Read events from disk, newest last, with optional filters.
/// `since_ms` of 0 = all. `since_seq` of 0 = all sequences.
pub fn read(
    since_ms: u64,
    workspace: Option<&str>,
    pane: Option<&str>,
    actor: Option<&str>,
    limit: usize,
) -> Vec<Event> {
    read_ex(since_ms, 0, workspace, pane, actor, None, limit)
}

/// Extended read with seq cursor and kind filter.
pub fn read_ex(
    since_ms: u64,
    since_seq: u64,
    workspace: Option<&str>,
    pane: Option<&str>,
    actor: Option<&str>,
    kinds: Option<&[String]>,
    limit: usize,
) -> Vec<Event> {
    let Ok(content) = std::fs::read_to_string(log_path()) else {
        return Vec::new();
    };
    let mut events: Vec<Event> = content
        .lines()
        .filter_map(|l| serde_json::from_str::<Event>(l).ok())
        .filter(|e| e.ts >= since_ms)
        .filter(|e| since_seq == 0 || e.seq > since_seq)
        .filter(|e| workspace.is_none_or(|w| e.workspace.as_deref() == Some(w)))
        .filter(|e| pane.is_none_or(|p| e.pane.as_deref() == Some(p)))
        .filter(|e| actor.is_none_or(|a| e.actor == a || e.actor.starts_with(&format!("{a}:"))))
        .filter(|e| {
            kinds.is_none_or(|ks| {
                ks.is_empty()
                    || ks
                        .iter()
                        .any(|k| k == &e.kind || e.kind.starts_with(k.as_str()))
            })
        })
        .collect();
    let skip = events.len().saturating_sub(limit);
    events.drain(..skip);
    events
}

/// How far back from the end of the log [`recent`] reads. A day of the
/// kinds the Wall asks for is well under this; the whole log can be far
/// bigger, and reading all of it every refresh is wasted work.
const TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Recent events of some kinds, oldest first, at most `limit` (the newest
/// kept), and the cursor to ask from next: the newest seq actually looked
/// at, so an event still being written is fetched next time, not skipped.
/// With a `since_seq` cursor the in-memory ring answers when it still holds
/// everything after it, so a client polling for new events costs no disk
/// read; otherwise the log's tail is read.
pub fn recent(since_ms: u64, since_seq: u64, kinds: &[String], limit: usize) -> (Vec<Event>, u64) {
    if since_seq > 0 {
        if let Some((events, cursor)) = ring_after(since_seq, since_ms, kinds) {
            return (trim_to_limit(events, limit), cursor);
        }
    }
    let path = log_path();
    let events = read_tail_at(&path, since_ms, since_seq, kinds, limit, TAIL_BYTES);
    let cursor = last_seq_at(&path).max(events.last().map_or(0, |e| e.seq));
    (events, cursor)
}

/// The ring's matching events after `since_seq` and the newest seq in it,
/// or None when the ring no longer reaches back that far (the daemon
/// restarted, or the ring rolled over). Filtered under the lock so the copy
/// is only what was asked for.
fn ring_after(since_seq: u64, since_ms: u64, kinds: &[String]) -> Option<(Vec<Event>, u64)> {
    let ring = bus().ring.lock().ok()?;
    let covered = match ring.front() {
        Some(first) => first.seq <= since_seq + 1,
        None => current_seq() == since_seq,
    };
    if !covered {
        return None;
    }
    let events = ring
        .iter()
        .filter(|e| e.seq > since_seq && e.ts >= since_ms)
        .filter(|e| matches_filter(e, None, None, None, Some(kinds)))
        .cloned()
        .collect();
    let cursor = ring.back().map_or(since_seq, |e| e.seq.max(since_seq));
    Some((events, cursor))
}

/// The seq on the log's last whole line (0 when there is none).
fn last_seq_at(path: &std::path::Path) -> u64 {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return 0;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)))
        .is_err()
        || f.read_to_end(&mut buf).is_err()
    {
        return 0;
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Event>(l).ok())
        .map_or(0, |e| e.seq)
}

/// [`read_ex`] over only the last `tail_bytes` of `path`, with a cheap text
/// check before each line is parsed: most lines are keystrokes nobody asked
/// for.
pub fn read_tail_at(
    path: &std::path::Path,
    since_ms: u64,
    since_seq: u64,
    kinds: &[String],
    limit: usize,
    tail_bytes: u64,
) -> Vec<Event> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(tail_bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    // A cut into the middle of a line leaves a fragment: skip it.
    let body = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let needles: Vec<String> = kinds.iter().map(|k| format!("\"kind\":\"{k}")).collect();
    let events = body
        .lines()
        .filter(|l| needles.is_empty() || needles.iter().any(|n| l.contains(n.as_str())))
        .filter_map(|l| serde_json::from_str::<Event>(l).ok())
        .filter(|e| e.ts >= since_ms && (since_seq == 0 || e.seq > since_seq))
        .filter(|e| matches_filter(e, None, None, None, Some(kinds)))
        .collect();
    trim_to_limit(events, limit)
}

fn trim_to_limit(mut events: Vec<Event>, limit: usize) -> Vec<Event> {
    let skip = events.len().saturating_sub(limit);
    events.drain(..skip);
    events
}

/// Unix ms of this machine's most recent local midnight.
pub fn local_day_start_ms(now_ms: u64) -> u64 {
    let secs = (now_ms / 1000) as i64;
    let local = secs + local_offset_secs(secs);
    let day = local - local.rem_euclid(86_400);
    // Midnight's own offset: on a clock-change day it isn't the one now.
    let midnight = day - local_offset_secs(day - local_offset_secs(secs));
    midnight.max(0) as u64 * 1000
}

/// The local calendar date of `ts_ms` as `YYYY-MM-DD`.
pub fn local_date(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as i64;
    let local = secs + local_offset_secs(secs);
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian
/// (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Does `event` match a watch filter?
pub fn matches_filter(
    e: &Event,
    workspace: Option<&str>,
    pane: Option<&str>,
    actor: Option<&str>,
    kinds: Option<&[String]>,
) -> bool {
    if let Some(w) = workspace {
        if e.workspace.as_deref() != Some(w) {
            return false;
        }
    }
    if let Some(p) = pane {
        if e.pane.as_deref() != Some(p) {
            return false;
        }
    }
    if let Some(a) = actor {
        if e.actor != a && !e.actor.starts_with(&format!("{a}:")) {
            return false;
        }
    }
    if let Some(ks) = kinds {
        if !ks.is_empty()
            && !ks
                .iter()
                .any(|k| k == &e.kind || e.kind.starts_with(k.as_str()))
        {
            return false;
        }
    }
    true
}

/// Render a timestamp as local wall-clock HH:MM:SS.
pub fn fmt_time(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as i64;
    let local = secs + local_offset_secs(secs);
    let (h, m, s) = (
        (local / 3600).rem_euclid(24),
        (local / 60).rem_euclid(60),
        local.rem_euclid(60),
    );
    format!("{h:02}:{m:02}:{s:02}")
}

/// Local (weekday counted from Sunday, hour, minute) of `ts_ms`.
pub fn local_clock(ts_ms: u64) -> (u32, u32, u32) {
    let secs = (ts_ms / 1000) as i64;
    clock_parts(secs + local_offset_secs(secs))
}

/// The weekday (0 = Sunday), hour and minute of `local` seconds since the
/// epoch, which was a Thursday.
fn clock_parts(local: i64) -> (u32, u32, u32) {
    let weekday = (local.div_euclid(86_400) + 4).rem_euclid(7) as u32;
    let day_secs = local.rem_euclid(86_400);
    (
        weekday,
        (day_secs / 3600) as u32,
        (day_secs % 3600 / 60) as u32,
    )
}

/// This machine's UTC offset in seconds at unix time `secs`, daylight saving
/// included. Asked of the C library every call, so a long-running daemon
/// follows the clock change.
fn local_offset_secs(secs: i64) -> i64 {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return 0;
    }
    i64::from(tm.tm_gmtoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_parts_know_the_weekday_hour_and_minute() {
        // 1970-01-01 was a Thursday; 1970-01-04 a Sunday.
        assert_eq!(clock_parts(0), (4, 0, 0));
        assert_eq!(clock_parts(3 * 86_400 + 13 * 3600 + 50 * 60), (0, 13, 50));
        assert_eq!(clock_parts(-60), (3, 23, 59));
    }

    #[test]
    fn local_offset_matches_date_now() {
        let now = (now_ms() / 1000) as i64;
        let out = std::process::Command::new("date")
            .arg("+%z")
            .output()
            .unwrap();
        let z = String::from_utf8(out.stdout).unwrap();
        let z = z.trim();
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let want =
            sign * (z[1..3].parse::<i64>().unwrap() * 3600 + z[3..5].parse::<i64>().unwrap() * 60);
        assert_eq!(local_offset_secs(now), want);
        // Far ends of the range still answer.
        local_offset_secs(0);
        local_offset_secs(4_102_444_800);
        let start = local_day_start_ms(now_ms());
        assert!(start <= now_ms() && now_ms() - start < 25 * 3600 * 1000);
    }

    #[test]
    fn log_assigns_seq_and_id() {
        // Isolated state dir so we don't pollute the real log, under the
        // lock every test that moves SEANCE_STATE_DIR shares.
        let _g = crate::state::test_env_lock();
        let prev = std::env::var("SEANCE_STATE_DIR").ok();
        let dir = std::env::temp_dir().join(format!("seance-events-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("SEANCE_STATE_DIR", &dir);
        // Note: bus is OnceLock — may already be init from other tests in-process.
        // Just check publish shape.
        let e = log("cli", Some("lab"), Some("w1"), "test_kind", "hello".into());
        assert!(!e.id.is_empty() || e.seq > 0 || e.ts > 0);
        assert_eq!(e.kind, "test_kind");
        assert_eq!(e.actor, "cli");
        match prev {
            Some(v) => std::env::set_var("SEANCE_STATE_DIR", v),
            None => std::env::remove_var("SEANCE_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matches_filter_kinds_prefix() {
        let e = Event {
            id: "evt_1".into(),
            seq: 1,
            ts: 1,
            actor: "agent:w".into(),
            workspace: Some("lab".into()),
            pane: Some("w".into()),
            kind: "ctl_send".into(),
            detail: "x".into(),
            caused_by: None,
            span: None,
            origin: Some("ctl_send".into()),
        };
        let kinds = vec!["ctl_".into()];
        assert!(matches_filter(&e, Some("lab"), None, None, Some(&kinds)));
        let kinds2 = vec!["status_set".into()];
        assert!(!matches_filter(&e, None, None, None, Some(&kinds2)));
    }

    #[test]
    fn civil_dates_and_midnights() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_745), (2026, 10, 19));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        let now = 1_792_439_251_966;
        let start = local_day_start_ms(now);
        assert!(start <= now && now - start < 86_400_000);
        assert_eq!(start % 1000, 0);
        assert_eq!(local_date(start), local_date(now));
        assert_ne!(local_date(start - 1), local_date(now));
    }

    #[test]
    fn tail_reads_skip_the_cut_line_and_filter_kinds() {
        let dir = std::env::temp_dir().join(format!("seance-tail-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("events.jsonl");
        let line = |seq: u64, ts: u64, kind: &str| {
            format!(
                r#"{{"id":"evt_{seq}","seq":{seq},"ts":{ts},"actor":"daemon","pane":"p","kind":"{kind}","detail":"d"}}"#
            )
        };
        let lines = [
            line(1, 100, "status_set"),
            line(2, 200, "terminal.input"),
            line(3, 300, "status_set"),
            line(4, 400, "pane_exited"),
            line(5, 500, "status_set"),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let kinds = vec!["status_set".to_string(), "pane_exited".to_string()];
        let seqs = |v: Vec<Event>| v.iter().map(|e| e.seq).collect::<Vec<_>>();
        assert_eq!(
            seqs(read_tail_at(&path, 0, 0, &kinds, 10, 1 << 20)),
            [1, 3, 4, 5]
        );
        // since_ms, since_seq and limit (newest kept).
        assert_eq!(
            seqs(read_tail_at(&path, 300, 0, &kinds, 10, 1 << 20)),
            [3, 4, 5]
        );
        assert_eq!(seqs(read_tail_at(&path, 0, 3, &kinds, 10, 1 << 20)), [4, 5]);
        assert_eq!(seqs(read_tail_at(&path, 0, 0, &kinds, 2, 1 << 20)), [4, 5]);
        // A tail that starts mid-line drops the fragment, keeps whole lines.
        let tail = (lines[3].len() + lines[4].len() + 2 + 5) as u64;
        assert_eq!(seqs(read_tail_at(&path, 0, 0, &kinds, 10, tail)), [4, 5]);
        assert!(read_tail_at(&dir.join("missing"), 0, 0, &kinds, 10, 1 << 20).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_event_deserializes() {
        let json = r#"{"ts":1,"actor":"human","workspace":"lab","pane":"t1","kind":"focus","detail":"focused"}"#;
        let e: Event = serde_json::from_str(json).unwrap();
        assert_eq!(e.kind, "focus");
        assert_eq!(e.seq, 0);
        assert!(e.id.is_empty());
    }
}
