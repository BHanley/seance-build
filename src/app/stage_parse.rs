//! Pure helpers behind Stage: the circle map, the lead log's newest entry,
//! the health report, pill overflow, the next-waiting ring, a pad's latest
//! finish note, crew order and a pane's last line.
//! No gpui here, so every rule is unit-tested.

/// The folder a circle's project lives in, from a `slug<TAB>folder` map.
/// `#` lines and blank lines are comments; the first match wins.
pub(super) fn circle_folder(map: &str, circle: &str) -> Option<String> {
    map.lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .find(|(slug, _)| slug.trim() == circle)
        .map(|(_, folder)| folder.trim().to_string())
        .filter(|f| !f.is_empty())
}

/// Where a circle's LEAD-LOG.md lives. A relative folder sits beside the
/// map file. Paths stay strings with `~` intact: the daemon expands them.
pub(super) fn lead_log_path(map_path: &str, folder: &str) -> String {
    let folder = folder.trim_end_matches('/');
    if folder.starts_with('/') || folder.starts_with('~') {
        return format!("{folder}/LEAD-LOG.md");
    }
    match map_path.rfind('/') {
        Some(i) => format!("{}/{folder}/LEAD-LOG.md", &map_path[..i]),
        None => format!("{folder}/LEAD-LOG.md"),
    }
}

/// Doing / Next / Waiting on from the newest dated entry of a lead log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LeadLine {
    pub doing: Option<String>,
    pub next: Option<String>,
    pub waiting: Option<String>,
}

/// Longest a Doing / Next / Waiting value gets before it is cut.
const LEAD_FIELD_MAX: usize = 90;

/// The top `## ` entry's `- **Doing:**`, `- **Next:**` and `- **Waiting
/// on:**` bullets (bold optional, as older entries wrote them). None when
/// the entry has none of the three.
pub(super) fn parse_lead_log(text: &str) -> Option<LeadLine> {
    let mut lines = text.lines().skip_while(|l| !l.starts_with("## "));
    lines.next()?;
    let mut out = LeadLine::default();
    for line in lines.take_while(|l| !l.starts_with("## ")) {
        let Some(item) = line.trim_start().strip_prefix("- ") else {
            continue;
        };
        let item = item.replace("**", "");
        let item = item.trim();
        let Some((key, value)) = item.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        // Older entries wrote "Waiting on Ann: ...": keep who it waits on.
        let mut value = value.trim().to_string();
        let slot = match key.as_str() {
            "doing" => &mut out.doing,
            "next" => &mut out.next,
            "waiting on" | "waiting" => &mut out.waiting,
            k if k.starts_with("waiting on ") => {
                let who = item.split_once(':').map_or("", |(k, _)| k)["waiting on ".len()..].trim();
                value = format!("{who}: {value}");
                &mut out.waiting
            }
            _ => continue,
        };
        let value = ellipsize(&strip_markdown(&value), LEAD_FIELD_MAX);
        if slot.is_none() && !value.is_empty() {
            *slot = Some(value);
        }
    }
    (out != LeadLine::default()).then_some(out)
}

/// Drop the markdown a one-line label can't show: backticks, emphasis and
/// link targets (`[text](url)` keeps the text).
pub(super) fn strip_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find("](") {
            Some(j) if !after[..j].contains('[') => {
                let tail = &after[j + 2..];
                match tail.find(')') {
                    Some(k) => {
                        out.push_str(&after[..j]);
                        rest = &tail[k + 1..];
                    }
                    None => {
                        out.push('[');
                        rest = after;
                    }
                }
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace(['`', '*'], "").trim().to_string()
}

/// Cut to `max` characters with an ellipsis, on a char boundary.
pub(super) fn ellipsize(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// How the health report's title reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HealthLevel {
    Healthy,
    Warning,
    Critical,
}

/// The footer's view of a health report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Health {
    pub level: HealthLevel,
    /// "healthy", "1 warning", ... : the title after its timestamp.
    pub status: String,
    /// "11:48" from the title.
    pub at: Option<String>,
    /// "2026-10-19" from the title, so the Wall's feed shows only today's.
    pub day: Option<String>,
    /// The machine the daemon runs on, named by its "<name> memory" line.
    pub host: Option<String>,
    /// "memory 34% free".
    pub host_memory: Option<String>,
    /// None = the report has no Mac line or says it is out of reach.
    pub mac: Option<String>,
    pub leads: Option<String>,
    /// "plan 24% of 5h".
    pub plan: Option<String>,
}

impl Health {
    /// The daemon machine's name for the footer: from the report, else "Host".
    pub(super) fn host_label(&self) -> &str {
        self.host.as_deref().unwrap_or("Host")
    }
}

/// Read the health check's `latest.md`. None when the title line is missing,
/// so a wrong file shows no footer rather than a made-up one.
pub(super) fn parse_health(text: &str) -> Option<Health> {
    let title = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("# Seance health"))?;
    // "2026-10-19 11:48: healthy, notes below"
    let (stamp, status) = title.split_once(": ").unwrap_or((title, ""));
    let at = stamp
        .split_whitespace()
        .last()
        .filter(|t| t.contains(':'))
        .map(str::to_string);
    let day = stamp
        .split_whitespace()
        .find(|t| t.len() == 10 && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-')
        .map(str::to_string);
    let status = status
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let level = if status.starts_with("healthy") || status == "ok" {
        HealthLevel::Healthy
    } else if ["critical", "down", "fail", "error"]
        .iter()
        .any(|w| status.contains(w))
    {
        HealthLevel::Critical
    } else {
        HealthLevel::Warning
    };
    let mut h = Health {
        level,
        status: if status.is_empty() {
            "unknown".into()
        } else {
            ellipsize(&status, 24)
        },
        at,
        day,
        host: None,
        host_memory: None,
        mac: None,
        leads: None,
        plan: None,
    };
    for line in text.lines() {
        let Some(item) = line.trim_start().strip_prefix("- ") else {
            continue;
        };
        let item = strip_markdown(item);
        let Some((key, value)) = item.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let key = key.trim();
        // The first "<host> memory" line names the daemon's machine.
        let host_key = key
            .strip_suffix(" memory")
            .map(str::trim)
            .filter(|n| !n.is_empty());
        match key {
            _ if h.host_memory.is_none() && host_key.is_some() => {
                h.host = host_key.map(str::to_string);
                h.host_memory = percent_before(value, "free")
                    .map(|p| format!("memory {p}% free"))
                    .or_else(|| percent_before(value, "used").map(|p| format!("memory {p}% used")));
            }
            "Mac" if h.mac.is_none() => {
                let lower = value.to_ascii_lowercase();
                let away = ["unreachable", "asleep", "offline", "down", "timed out"]
                    .iter()
                    .any(|w| lower.contains(w));
                if !away {
                    h.mac = Some(match percent_after(value, "memory ") {
                        Some(p) => format!("Mac up · memory {p}% free"),
                        None => "Mac up".into(),
                    });
                }
            }
            "Leads" if h.leads.is_none() => {
                let lower = value.to_ascii_lowercase();
                h.leads = Some(if lower.starts_with("every") {
                    "every circle has a lead".into()
                } else {
                    ellipsize(&format!("leads: {value}"), 48)
                });
            }
            "Seance" if h.plan.is_none() => {
                h.plan = percent_after(value, "plan used ").map(|p| {
                    let window = value
                        .split("plan used ")
                        .nth(1)
                        .and_then(|r| r.split_once(" of "))
                        .and_then(|(_, r)| r.split(" limit").next())
                        .map(|w| w.replace(" hour", "h").replace(" day", "d"));
                    match window {
                        Some(w) if w.len() <= 6 => format!("Claude plan {p}% of {w}"),
                        _ => format!("Claude plan {p}%"),
                    }
                });
            }
            _ => {}
        }
    }
    Some(h)
}

/// The number in "(34%)" nearest before `word`: "2.5G of 7.4G free (34%)".
fn percent_before(s: &str, word: &str) -> Option<String> {
    let i = s.find(word)?;
    let tail = &s[i..];
    let open = tail.find('(')?;
    let close = tail[open..].find("%)")?;
    let n = &tail[open + 1..open + close];
    n.parse::<f32>().ok().map(|_| n.to_string())
}

/// The number right after `prefix`, up to its `%`: "memory 34% free".
fn percent_after(s: &str, prefix: &str) -> Option<String> {
    let start = s.find(prefix)? + prefix.len();
    let tail = &s[start..];
    let end = tail.find('%')?;
    let n = tail[..end].trim();
    n.parse::<f32>().ok().map(|_| n.to_string())
}

/// Which pills fit in `avail` px, in rail order, plus room for a "+N more"
/// pill when some don't. The selected circle always keeps its pill.
pub(super) fn fit_pills(
    widths: &[f32],
    avail: f32,
    more_w: f32,
    gap: f32,
    selected: Option<usize>,
) -> Vec<usize> {
    let total: f32 = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    if total <= avail {
        return (0..widths.len()).collect();
    }
    let mut budget = avail - more_w - gap;
    let mut keep = vec![false; widths.len()];
    if let Some(s) = selected.filter(|s| *s < widths.len()) {
        keep[s] = true;
        budget -= widths[s] + gap;
    }
    for (i, w) in widths.iter().enumerate() {
        if keep[i] {
            continue;
        }
        if w + gap > budget {
            break;
        }
        keep[i] = true;
        budget -= w + gap;
    }
    (0..widths.len()).filter(|i| keep[*i]).collect()
}

/// A pill's width estimate: padding, the status glyph and the label.
pub(super) fn pill_width(label: &str) -> f32 {
    24. + 14. + label.chars().count() as f32 * 6.9
}

/// ctrl+alt+page: the next space in `delta`'s direction that has circles,
/// wrapping. `counts` is circles per space in space order; `cur` is the
/// space on show. None when no other space has any.
pub(super) fn step_space(counts: &[usize], cur: Option<usize>, delta: i32) -> Option<usize> {
    let n = counts.len() as i32;
    let step = delta.signum();
    if n == 0 || step == 0 {
        return None;
    }
    let start = cur.map_or(if step > 0 { -1 } else { n }, |c| c as i32);
    (1..=n)
        .map(|k| (start + step * k).rem_euclid(n) as usize)
        .find(|&i| Some(i) != cur && counts[i] > 0)
}

/// The needs-you bar's done chips: each circle in `order` that `unread`
/// marks done, with the pane that finished. `unseen` (Claude panes that
/// finished out of sight) wins over `status_done` (a ctl "done" status,
/// which can be old); both are circle and slug in pane order.
pub(super) fn done_chips(
    order: &[String],
    unread: &[&str],
    unseen: &[(&str, &str)],
    status_done: &[(&str, &str)],
) -> Vec<(String, Option<String>)> {
    let first = |list: &[(&str, &str)], c: &str| {
        list.iter()
            .find(|(ws, _)| *ws == c)
            .map(|(_, slug)| slug.to_string())
    };
    order
        .iter()
        .filter(|c| unread.contains(&c.as_str()))
        .map(|c| {
            let pane = first(unseen, c).or_else(|| first(status_done, c));
            (c.clone(), pane)
        })
        .collect()
}

/// The next-waiting key's ring: circle and pane of each needs-you bar chip,
/// in bar order. Chips with no pane (a ctl ask from outside one) can't be
/// jumped to, and a pane listed twice is visited once.
pub(super) fn jump_ring(chips: &[(&str, Option<&str>)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (circle, pane) in chips {
        let Some(pane) = pane else { continue };
        if !out.iter().any(|(c, p)| c == circle && p == pane) {
            out.push((circle.to_string(), pane.to_string()));
        }
    }
    out
}

/// Which ring item the next-waiting key opens. `last` is the one it opened
/// before and `after` the items that followed it then: opening a done item
/// clears it from the ring, so the press after it picks up with whatever
/// followed it that is still waiting, else wraps to the top. `cur` (the
/// circle and pane on show) is stepped over.
pub(super) fn next_waiting<K: PartialEq>(
    ring: &[K],
    last: Option<&K>,
    after: &[K],
    cur: Option<&K>,
) -> Option<usize> {
    let n = ring.len();
    if n == 0 {
        return None;
    }
    let i = match last.and_then(|l| ring.iter().position(|k| k == l)) {
        Some(i) => (i + 1) % n,
        None => after
            .iter()
            .find_map(|a| ring.iter().position(|k| k == a))
            .unwrap_or(0),
    };
    if n > 1 && cur == Some(&ring[i]) {
        return Some((i + 1) % n);
    }
    Some(i)
}

/// The newest `ctl finish` note in a pane's scratchpad: the lines under the
/// last `<!-- author · time · finish -->` header, up to the next section's
/// `---` and header, at most `max` lines with "…" when cut. None when the
/// pad has no finish note or it is empty.
pub(super) fn latest_finish(pad: &str, max: usize) -> Option<Vec<String>> {
    let is_header = |l: &str| {
        let t = l.trim();
        t.starts_with("<!--") && t.ends_with("-->")
    };
    let lines: Vec<&str> = pad.lines().collect();
    let start = lines.iter().rposition(|l| finish_time(l).is_some())? + 1;
    let mut body: Vec<&str> = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(start) {
        if line.trim() == "---" && lines.get(i + 1).is_some_and(|n| is_header(n)) {
            break;
        }
        body.push(line.trim_end());
    }
    while body.first().is_some_and(|l| l.is_empty()) {
        body.remove(0);
    }
    while body.last().is_some_and(|l| l.is_empty()) {
        body.pop();
    }
    if body.is_empty() || max == 0 {
        return None;
    }
    let cut = body.len() > max;
    let mut out: Vec<String> = body
        .into_iter()
        .take(if cut { max - 1 } else { max })
        .map(str::to_string)
        .collect();
    if cut {
        out.push("…".into());
    }
    Some(out)
}

/// When the newest finish note was filed, as "12:16".
pub(super) fn latest_finish_at(pad: &str) -> Option<String> {
    let t = pad.lines().rev().find_map(finish_time)?;
    Some(t.rsplit_once(':').map_or(t, |(hm, _)| hm).to_string())
}

/// The peek card's "report filed" label. Headers carry no date, so a time
/// later than now (`HH:MM` both) can only be from an earlier day.
pub(super) fn filed_label(filed: &str, now: &str) -> String {
    if filed > now {
        format!("report filed {filed}, an earlier day")
    } else {
        format!("report filed {filed}")
    }
}

/// Whether the hook's reply line (markdown marks stripped, trimmed, cut
/// at 140 chars) is just the finish note's first line again.
pub(super) fn reply_repeats(reply: &str, first: &str) -> bool {
    let first: String = first
        .chars()
        .filter(|c| !matches!(c, '*' | '#' | '`'))
        .collect();
    let reply = reply.trim();
    !reply.is_empty() && first.trim().starts_with(reply)
}

/// A finish section's header (`<!-- who · 12:16:49 · finish -->`): its time.
fn finish_time(l: &str) -> Option<&str> {
    let t = l.trim().strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let mut parts = t.rsplit(" · ");
    parts.next().filter(|p| *p == "finish")?;
    parts.next().map(str::trim)
}

/// A crew member's sort band: waiting on you, then working, then finished,
/// then quiet, then shells and sleepers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum CrewBand {
    Needs,
    Working,
    Done,
    Idle,
    Quiet,
}

/// Crew order: by band, then the circle's own pane order, so cards only move
/// when a pane's state changes.
pub(super) fn crew_order(mut crew: Vec<(usize, CrewBand)>) -> Vec<usize> {
    crew.sort_by_key(|(i, band)| (*band, *i));
    crew.into_iter().map(|(i, _)| i).collect()
}

/// A plain shell with nothing running in it: drawn as a dashed card.
pub(super) fn is_bare_shell(command: &str, title: Option<&str>, busy: bool) -> bool {
    if busy {
        return false;
    }
    if title.is_some_and(|t| t.starts_with('✳') || super::util::title_looks_busy(t)) {
        return false;
    }
    let prog = command
        .split_whitespace()
        .find(|t| *t != "env" && !t.contains('='))
        .and_then(|t| t.rsplit('/').next())
        .unwrap_or("");
    matches!(prog, "" | "bash" | "zsh" | "sh" | "fish" | "nu")
}

/// Split "lead · Mac" into the name and its host chip. Seance's pane names
/// carry the machine as a short last word after " · "; `claude-mac` panes
/// are on the Mac even when the name doesn't say so.
pub(super) fn pane_host<'a>(name: &'a str, command: &str) -> (&'a str, Option<&'a str>) {
    if let Some((base, host)) = name.rsplit_once(" · ") {
        let host = host.trim();
        if !host.is_empty() && host.len() <= 8 && !host.contains(' ') && !base.trim().is_empty() {
            return (base.trim(), Some(host));
        }
    }
    let mac = command
        .split_whitespace()
        .any(|t| t.rsplit('/').next() == Some("claude-mac"));
    (name, mac.then_some("Mac"))
}

/// The pane's last `n` lines of real output, oldest first, skipping blank
/// lines and a TUI's input box at the bottom (rules, the `❯` prompt, the
/// hints under it).
pub(super) fn last_output_lines(rows: &[String], n: usize) -> Vec<String> {
    let is_rule = |l: &str| {
        let t = l.trim();
        t.chars().count() >= 3
            && t.chars()
                .all(|c| ('\u{2500}'..='\u{257F}').contains(&c) || c == ' ')
    };
    // A rule near the bottom opens the input box: read above the highest
    // one there.
    let floor = rows.len().saturating_sub(10);
    let end = rows[floor..]
        .iter()
        .position(|l| is_rule(l))
        .map(|i| floor + i)
        .unwrap_or(rows.len());
    let mut out: Vec<String> = rows[..end]
        .iter()
        .rev()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !is_rule(l))
        .take(n)
        .map(|l| ellipsize(l, 120))
        .collect();
    out.reverse();
    out
}

/// Claude Code's idle compaction ("Compacted while idle, before the prompt
/// cache expired") wears the spinner title like a turn. True when that note
/// is in the newest block of `lines` (from its last `⏺`/`●` reply or `>`/`❯`
/// prompt down), so a real turn after it still counts.
pub(super) fn compacted_while_idle(lines: &[String]) -> bool {
    let start = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with(['⏺', '●', '>', '❯']))
        .unwrap_or(0);
    lines[start..].iter().any(|l| {
        l.trim_start_matches(['⏺', '●', ' '])
            .starts_with("Compacted while idle")
    })
}

/// One Claude plan limit from the usage file: percent used and when it resets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UsageWindow {
    pub used: u32,
    /// Unix seconds.
    pub resets_at: u64,
}

/// The footer's usage bars: the 5 hour and weekly limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Usage {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    /// Unix seconds of the newest reading.
    pub at: u64,
}

/// Read `seance-usage-file`: `{"five_hour": {"used": 9, "resets_at": 1793403000}, ...}`.
/// None for a file with neither window.
pub(super) fn parse_usage(text: &str) -> Option<Usage> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let window = |k: &str| {
        let w = v.get(k)?;
        Some(UsageWindow {
            used: u32::try_from(w.get("used")?.as_u64()?).ok()?,
            resets_at: w.get("resets_at")?.as_u64()?,
        })
    };
    let u = Usage {
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
        at: v.get("at").and_then(|a| a.as_u64()).unwrap_or(0),
    };
    (u.five_hour.is_some() || u.seven_day.is_some()).then_some(u)
}

/// What a window shows at `now`: a reset that has passed reads as 0% used.
pub(super) fn usage_now(w: UsageWindow, now: u64) -> (u32, u64) {
    if now >= w.resets_at {
        (0, 0)
    } else {
        (w.used, w.resets_at - now)
    }
}

/// Time to a reset, like the pane status line: "6d14h", "2h5m", "52m".
pub(super) fn reset_in(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d{}h", secs / 86_400, secs % 86_400 / 3_600)
    } else if secs >= 3_600 {
        format!("{}h{}m", secs / 3_600, secs % 3_600 / 60)
    } else {
        format!("{}m", secs / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_reads_both_windows_and_skips_junk() {
        let u = parse_usage(
            r#"{"five_hour":{"used":9,"resets_at":1000},"seven_day":{"used":13,"resets_at":5000},"at":1}"#,
        )
        .unwrap();
        assert_eq!(
            u.five_hour,
            Some(UsageWindow {
                used: 9,
                resets_at: 1000
            })
        );
        assert_eq!(u.seven_day.map(|w| w.used), Some(13));
        assert_eq!(parse_usage(r#"{"five_hour":{"used":9}}"#), None);
        assert_eq!(parse_usage("not json"), None);
    }

    #[test]
    fn usage_after_its_reset_reads_empty() {
        let w = UsageWindow {
            used: 62,
            resets_at: 1000,
        };
        assert_eq!(usage_now(w, 400), (62, 600));
        assert_eq!(usage_now(w, 1000), (0, 0));
        assert_eq!(reset_in(600), "10m");
        assert_eq!(reset_in(2 * 3600 + 5 * 60), "2h5m");
        assert_eq!(reset_in(6 * 86_400 + 14 * 3600), "6d14h");
    }

    const MAP: &str = "# circle slug<TAB>project folder under Projects/.\n\
main\tProduct Management\n\
alpha-circle\tAlpha Project\n\
\n\
circle-2\tBeta Project\n";

    #[test]
    fn circle_map_finds_folders_and_skips_comments() {
        assert_eq!(
            circle_folder(MAP, "alpha-circle").as_deref(),
            Some("Alpha Project")
        );
        assert_eq!(
            circle_folder(MAP, "circle-2").as_deref(),
            Some("Beta Project")
        );
        assert_eq!(circle_folder(MAP, "nope"), None);
        assert_eq!(
            circle_folder(MAP, "# circle slug<TAB>project folder under Projects/."),
            None
        );
        assert_eq!(circle_folder("x\t\n", "x"), None);
        assert_eq!(circle_folder("x Projects\n", "x"), None);
    }

    #[test]
    fn lead_log_path_sits_beside_the_map_and_keeps_tilde() {
        assert_eq!(
            lead_log_path("~/projects/circles.tsv", "Alpha Project"),
            "~/projects/Alpha Project/LEAD-LOG.md"
        );
        assert_eq!(lead_log_path("circles.tsv", "a"), "a/LEAD-LOG.md");
        assert_eq!(lead_log_path("/x/m.tsv", "/abs/p/"), "/abs/p/LEAD-LOG.md");
        assert_eq!(lead_log_path("/x/m.tsv", "~/p"), "~/p/LEAD-LOG.md");
    }

    const LOG: &str = "# Alpha Project lead log\n\
\n\
The circle lead's running log, newest first. Doing / Done / Next / Waiting on.\n\
\n\
## 2026-10-19 (The Oracle, pane alpha-circle)\n\
- **Done:** lead records saved for User Profile.\n\
- **Doing:** Seance looks: phase 1 themes on `bh/looks-themes` (worktree seance-looks). Plan: [the plan](https://x.y/z)\n\
- **Next:** both reviews on phase 1\n\
- **Waiting on:** nothing from you until phase 1 is reviewed.\n\
\n\
## 2026-10-16 (12:20, The Oracle, pane alpha-circle)\n\
- **Doing:** an older thing\n";

    #[test]
    fn lead_log_reads_the_newest_entry_only() {
        let l = parse_lead_log(LOG).unwrap();
        assert_eq!(
            l.doing.as_deref(),
            Some("Seance looks: phase 1 themes on bh/looks-themes (worktree seance-looks). Plan: the plan")
        );
        assert_eq!(l.next.as_deref(), Some("both reviews on phase 1"));
        assert_eq!(
            l.waiting.as_deref(),
            Some("nothing from you until phase 1 is reviewed.")
        );
    }

    #[test]
    fn lead_log_missing_fields_and_plain_bullets() {
        let l = parse_lead_log("## 2026-10-15\n- Done: log started\n- Next: pick up\n").unwrap();
        assert_eq!(l.doing, None);
        assert_eq!(l.next.as_deref(), Some("pick up"));
        assert_eq!(l.waiting, None);
        // An entry with none of the three hides the line.
        assert_eq!(parse_lead_log("## 2026-10-15\n- Done: only done\n"), None);
        assert_eq!(parse_lead_log("no entries at all\n- **Doing:** x\n"), None);
        assert_eq!(parse_lead_log(""), None);
        let l = parse_lead_log("## d\n- Waiting on Ann: re-try a box\n").unwrap();
        assert_eq!(l.waiting.as_deref(), Some("Ann: re-try a box"));
        // `**Doing**:` puts the colon outside the bold.
        let l = parse_lead_log("## d\n- **Doing**: bold outside\n").unwrap();
        assert_eq!(l.doing.as_deref(), Some("bold outside"));
    }

    #[test]
    fn lead_fields_are_cut_to_one_short_line() {
        let long = format!("## d\n- **Doing:** {}\n", "word ".repeat(40));
        let doing = parse_lead_log(&long).unwrap().doing.unwrap();
        assert!(doing.chars().count() <= LEAD_FIELD_MAX, "{doing}");
        assert!(doing.ends_with('…'));
    }

    #[test]
    fn markdown_is_stripped_for_labels() {
        assert_eq!(
            strip_markdown("run `cargo test` **now**"),
            "run cargo test now"
        );
        assert_eq!(
            strip_markdown("see [PR 12](https://g/12) ok"),
            "see PR 12 ok"
        );
        assert_eq!(strip_markdown("a [b] c"), "a [b] c");
        assert_eq!(strip_markdown("[x](unclosed"), "[x](unclosed");
    }

    const HEALTH: &str = "# Seance health 2026-10-19 11:48: healthy, notes below\n\
\n\
- **info** Personal still says 'working' but is idle\n\
\n\
- Devbox memory: 2.5G of 7.4G free (34%), swap 5.3G used (36%)\n\
- Mac: load 5.3 on 10 cores, memory 34% free, swap 6.4G, disk 4% used, 7 Claude processes\n\
- Seance: 32 panes in 11 circles; Claude plan used 24% of 5 hour limit, 14% of 7 day limit\n\
- Leads: every project circle has a live Claude lead\n";

    #[test]
    fn health_report_reads_every_footer_field() {
        let h = parse_health(HEALTH).unwrap();
        assert_eq!(h.level, HealthLevel::Healthy);
        assert_eq!(h.status, "healthy");
        assert_eq!(h.at.as_deref(), Some("11:48"));
        assert_eq!(h.day.as_deref(), Some("2026-10-19"));
        assert_eq!(h.host.as_deref(), Some("Devbox"));
        assert_eq!(h.host_memory.as_deref(), Some("memory 34% free"));
        assert_eq!(h.mac.as_deref(), Some("Mac up · memory 34% free"));
        assert_eq!(h.leads.as_deref(), Some("every circle has a lead"));
        assert_eq!(h.plan.as_deref(), Some("Claude plan 24% of 5h"));
    }

    #[test]
    fn health_report_handles_a_sleeping_mac_and_problems() {
        let text = "# Seance health 2026-10-19 12:00: 2 warnings\n\
- Mac: unreachable (ssh timed out)\n\
- Leads: 2 circles have no lead: garmin-nav, pm-odd-jobs and more after that\n";
        let h = parse_health(text).unwrap();
        assert_eq!(h.level, HealthLevel::Warning);
        assert_eq!(h.status, "2 warnings");
        assert_eq!(h.mac, None);
        assert!(h.leads.as_deref().unwrap().starts_with("leads: 2 circles"));
        assert!(h.leads.unwrap().chars().count() <= 48);
        assert_eq!(h.host, None);
        assert_eq!(h.host_memory, None);
        assert_eq!(h.plan, None);

        let crit =
            parse_health("# Seance health 2026-10-19 12:00: critical, daemon down\n").unwrap();
        assert_eq!(crit.level, HealthLevel::Critical);
        // No Mac line at all reads as asleep too.
        assert_eq!(crit.mac, None);
    }

    #[test]
    fn health_report_needs_its_title() {
        assert_eq!(parse_health(""), None);
        assert_eq!(parse_health("- Devbox memory: 1G of 2G free (50%)\n"), None);
        let bare = parse_health("# Seance health\n").unwrap();
        assert_eq!(bare.status, "unknown");
        assert_eq!(bare.at, None);
        assert_eq!(bare.day, None);
    }

    #[test]
    fn pills_overflow_into_more_and_keep_the_selection() {
        let w = [100., 100., 100., 100.];
        // Everything fits: no "+N more".
        assert_eq!(fit_pills(&w, 430., 60., 6., None), vec![0, 1, 2, 3]);
        // 300 px: two pills plus the more pill.
        assert_eq!(fit_pills(&w, 300., 60., 6., None), vec![0, 1]);
        // The selected circle stays even when it is last in the rail.
        assert_eq!(fit_pills(&w, 300., 60., 6., Some(3)), vec![0, 3]);
        // Too narrow for anything but the selection.
        assert_eq!(fit_pills(&w, 120., 60., 6., Some(2)), vec![2]);
        assert_eq!(fit_pills(&[], 10., 60., 6., None), Vec::<usize>::new());
        // A wide pill stops the run; later narrow ones don't jump ahead of it.
        assert_eq!(fit_pills(&[50., 400., 50.], 300., 60., 6., None), vec![0]);
    }

    #[test]
    fn space_steps_skip_empty_spaces_and_wrap() {
        let counts = [2, 0, 3, 1];
        assert_eq!(step_space(&counts, Some(0), 1), Some(2));
        assert_eq!(step_space(&counts, Some(2), 1), Some(3));
        assert_eq!(step_space(&counts, Some(3), 1), Some(0));
        assert_eq!(step_space(&counts, Some(0), -1), Some(3));
        assert_eq!(step_space(&counts, Some(2), -1), Some(0));
        // Showing an empty space: step off it either way.
        assert_eq!(step_space(&counts, Some(1), 1), Some(2));
        assert_eq!(step_space(&counts, Some(1), -1), Some(0));
        // Nothing on show: the first or last space with circles.
        assert_eq!(step_space(&counts, None, 1), Some(0));
        assert_eq!(step_space(&counts, None, -1), Some(3));
        // No other space has circles.
        assert_eq!(step_space(&[4, 0], Some(0), 1), None);
        assert_eq!(step_space(&[4], Some(0), -1), None);
        assert_eq!(step_space(&[], None, 1), None);
    }

    #[test]
    fn done_chips_follow_circle_order_and_find_the_done_pane() {
        let order: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let chips = done_chips(
            &order,
            &["c", "a", "b"],
            &[("b", "b-2")],
            &[("b", "b-1"), ("c", "c-2"), ("c", "c-3")],
        );
        // An old ctl "done" on b-1 loses to b-2, which just finished.
        assert_eq!(
            chips,
            vec![
                ("a".to_string(), None),
                ("b".to_string(), Some("b-2".to_string())),
                ("c".to_string(), Some("c-2".to_string()))
            ]
        );
        assert!(done_chips(&order, &[], &[], &[("a", "a-1")]).is_empty());
        // A circle that is gone has no chip.
        assert!(done_chips(&order, &["z"], &[], &[]).is_empty());
    }

    #[test]
    fn jump_ring_skips_paneless_chips_and_repeats() {
        let ring = jump_ring(&[
            ("a", Some("a-1")),
            ("", None),
            ("b", Some("b-2")),
            ("a", Some("a-1")),
            ("c", Some("c-1")),
        ]);
        let want: Vec<(String, String)> = [("a", "a-1"), ("b", "b-2"), ("c", "c-1")]
            .iter()
            .map(|(c, p)| (c.to_string(), p.to_string()))
            .collect();
        assert_eq!(ring, want);
        assert!(jump_ring(&[("x", None)]).is_empty());
    }

    #[test]
    fn next_waiting_walks_the_ring_and_wraps() {
        let ring = ["a", "b", "c"];
        // First press: the top, unless you're already on it.
        assert_eq!(next_waiting(&ring, None, &[], None), Some(0));
        assert_eq!(next_waiting(&ring, None, &[], Some(&"a")), Some(1));
        // Then the one after the last, wrapping.
        assert_eq!(
            next_waiting(&ring, Some(&"a"), &["b", "c"], Some(&"a")),
            Some(1)
        );
        assert_eq!(next_waiting(&ring, Some(&"c"), &[], Some(&"c")), Some(0));
        // One item: open it again.
        assert_eq!(next_waiting(&["a"], Some(&"a"), &[], Some(&"a")), Some(0));
        assert_eq!(next_waiting::<&str>(&[], None, &[], None), None);
    }

    #[test]
    fn next_waiting_does_not_skip_when_the_ring_shrinks() {
        // b was a done chip: opening it cleared it, so the ring lost it.
        let ring = ["a", "c", "d"];
        assert_eq!(
            next_waiting(&ring, Some(&"b"), &["c", "d"], Some(&"b")),
            Some(1)
        );
        // c went too: d is next.
        assert_eq!(
            next_waiting(&["a", "d"], Some(&"b"), &["c", "d"], None),
            Some(1)
        );
        // Nothing that followed it is left: wrap to the top.
        assert_eq!(next_waiting(&["a"], Some(&"b"), &["c"], None), Some(0));
    }

    const PAD: &str = "my notes\n\
\n\
---\n\
<!-- agent:x · 10:09:00 · finish -->\n\
\n\
old finish\n\
\n\
---\n\
<!-- agent:x · 12:16:49 · finish -->\n\
\n\
Shipped it.\n\
- tests pass\n\
\n\
- not relaunched\n\
\n\
\n\
---\n\
<!-- human · 12:20:00 -->\n\
\n\
a later note\n";

    #[test]
    fn latest_finish_reads_the_newest_finish_section() {
        assert_eq!(
            latest_finish(PAD, 12).unwrap(),
            ["Shipped it.", "- tests pass", "", "- not relaunched"]
        );
        // A line of dashes inside the note isn't a section break.
        let pad = "---\n<!-- a · 1 · finish -->\nx\n---\ny\n";
        assert_eq!(latest_finish(pad, 12).unwrap(), ["x", "---", "y"]);
        assert_eq!(
            latest_finish(PAD, 3).unwrap(),
            ["Shipped it.", "- tests pass", "…"]
        );
        assert_eq!(
            latest_finish("plain pad\n---\n<!-- a · 1 -->\nnote\n", 12),
            None
        );
        assert_eq!(latest_finish("---\n<!-- a · 1 · finish -->\n\n", 12), None);
        assert_eq!(latest_finish("", 12), None);
        assert_eq!(latest_finish_at(PAD).as_deref(), Some("12:16"));
        assert_eq!(latest_finish_at("plain pad\n<!-- a · 1 -->\n"), None);
        assert_eq!(filed_label("12:16", "21:30"), "report filed 12:16");
        assert_eq!(
            filed_label("22:40", "09:05"),
            "report filed 22:40, an earlier day"
        );
        assert!(reply_repeats("Done: shipped it", "**Done:** shipped it"));
        assert!(reply_repeats("Shipped", "## Shipped"));
        assert!(!reply_repeats("Something newer", "Shipped it."));
        assert!(!reply_repeats("", "Shipped it."));
    }

    #[test]
    fn crew_sorts_by_band_then_pane_order() {
        let order = crew_order(vec![
            (0, CrewBand::Quiet),
            (1, CrewBand::Working),
            (2, CrewBand::Needs),
            (3, CrewBand::Working),
            (4, CrewBand::Done),
            (5, CrewBand::Idle),
        ]);
        assert_eq!(order, vec![2, 1, 3, 4, 5, 0]);
    }

    #[test]
    fn bare_shells_are_shells_with_nothing_running() {
        assert!(is_bare_shell("bash -l", Some("demo@devbox:~"), false));
        assert!(is_bare_shell("", None, false));
        assert!(is_bare_shell("/usr/bin/zsh", None, false));
        assert!(!is_bare_shell("bash -l", Some("✳ Claude Code"), false));
        assert!(!is_bare_shell("bash -l", None, true));
        assert!(!is_bare_shell("claude", None, false));
        assert!(!is_bare_shell("env A=1 claude-mac", None, false));
    }

    #[test]
    fn host_chip_comes_from_the_name_or_the_command() {
        assert_eq!(pane_host("lead · Mac", "bash"), ("lead", Some("Mac")));
        assert_eq!(
            pane_host("Follow Lead · devbox", "bash"),
            ("Follow Lead", Some("devbox"))
        );
        assert_eq!(pane_host("term-3", "claude-mac"), ("term-3", Some("Mac")));
        assert_eq!(pane_host("term-3", "bash -l"), ("term-3", None));
        // Long tails are part of the name, not a host.
        assert_eq!(
            pane_host("a · some long words", "bash"),
            ("a · some long words", None)
        );
    }

    fn last_output_line(rows: &[String]) -> Option<String> {
        last_output_lines(rows, 1).pop()
    }

    fn rows(s: &[&str]) -> Vec<String> {
        s.iter().map(|r| r.to_string()).collect()
    }

    #[test]
    fn last_line_skips_blanks_and_the_input_box() {
        let claude = rows(&[
            "● Read src/app/sidebar.rs",
            "  Three ideas, ranked",
            "✻ Cooked for 48s",
            "",
            "────────────────────",
            "❯ ",
            "────────────────────",
            "  ? for shortcuts",
        ]);
        assert_eq!(
            last_output_line(&claude).as_deref(),
            Some("✻ Cooked for 48s")
        );
        let shell = rows(&[
            "$ cargo build",
            "   Finished release",
            "demo@devbox:~$ ",
            "",
            "",
        ]);
        assert_eq!(last_output_line(&shell).as_deref(), Some("demo@devbox:~$"));
        assert_eq!(
            last_output_lines(&claude, 2),
            ["  Three ideas, ranked".trim(), "✻ Cooked for 48s"]
        );
        assert_eq!(last_output_lines(&shell, 5).len(), 3);
        assert_eq!(last_output_line(&rows(&["", "   "])), None);
        assert_eq!(last_output_line(&[]), None);
    }

    #[test]
    fn idle_compaction_is_not_a_turn() {
        let compacted = rows(&[
            "> ship it",
            "⏺ Shipped.",
            "⏺ Compacted while idle, before the prompt cache expired",
            "  ⎿  Read src/app/mod.rs (120 lines)",
        ]);
        assert!(compacted_while_idle(&compacted));
        let after = rows(&[
            "⏺ Compacted while idle, before the prompt cache expired",
            "> and the docs",
            "⏺ Docs updated.",
        ]);
        assert!(!compacted_while_idle(&after));
        assert!(!compacted_while_idle(&rows(&[
            "⏺ Done.",
            "✻ Cooked for 48s"
        ])));
        assert!(!compacted_while_idle(&[]));
        // A reply that only mentions it is a turn.
        let quoted = rows(&["⏺ Panes showing \"Compacted while idle\" no longer ding."]);
        assert!(!compacted_while_idle(&quoted));
    }

    #[test]
    fn ellipsize_counts_chars_not_bytes() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("ééééééé", 4), "ééé…");
        assert_eq!(ellipsize("ab cd ef", 4), "ab…");
    }
}
