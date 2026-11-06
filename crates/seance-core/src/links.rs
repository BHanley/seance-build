//! URL detection over a rendered grid — what a ctrl+click (native) or a tap
//! (web) resolves to — plus the inline code span a double-click copies and
//! the file path a double-click shows in Finder.
//!
//! Shared so the native GUI and the web client cannot drift, same rule as
//! `util::title_looks_busy`.

use crate::snapshot::{CellSnap, GridSnapshot};

/// `CellSnap::fg`/`bg` when the program set no colour.
const DEFAULT_COLOR: u32 = 0xFFFF_FFFF;

/// Rows joined either side of the hit row while each one continues. A
/// snapshot carries no WRAPPED flag, so continuation is inferred (see
/// [`continues`]); the cap bounds a screen of box-drawing from being glued
/// into one line.
const STITCH_MAX: u16 = 32;

fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
}

/// The link under one cell: an OSC-8 span covering it, else a bare http(s) URL
/// whose extent covers that column.
///
/// Cell-targeted on purpose. This used to open the first link anywhere on
/// screen and ignore the position, so ctrl+clicking a URL in a pane that also
/// had a PR reference higher up opened the PR.
pub fn url_at_cell(snap: &GridSnapshot, row: u16, col: u16) -> Option<String> {
    if let Some(h) = snap
        .hyperlinks
        .iter()
        .find(|h| h.row == row && col >= h.col_start && col < h.col_end)
    {
        return Some(h.uri.clone());
    }
    let (line, hit) = logical_line(snap, row, col)?;
    http_url_at(&line, hit)
}

/// How row `r` flows into row `r + 1`, if it does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wrap {
    /// The terminal wrapped: the row is full to its last column. Joined raw.
    Soft,
    /// The program wrapped (Claude/ink break a long line at their text-box
    /// width, short of the edge, and indent the rest): the row's text ends in
    /// the middle of a URL and the next row, after its indent, carries on
    /// with a URL-shaped token. Joined without the gap.
    Hard,
}

fn row_chars(snap: &GridSnapshot, r: u16) -> &[CellSnap] {
    let cols = snap.cols as usize;
    &snap.cells[r as usize * cols..(r as usize + 1) * cols]
}

/// Does row `r` carry on into `r + 1`?
///
/// The hard case is a guess from the text alone, tuned to never glue a URL
/// onto prose: row `r` must END in URL characters (no closing `.`, `)` …)
/// and row `r + 1` must OPEN with a token of URL characters that is not a
/// plain word (it has a digit, `/`, `.` …) — `…/ri` + `dewithgps/pull/1`
/// joins, `…/x` + `tra text` does not. Only the URL covering the hit is ever
/// returned, so a join elsewhere on the row is harmless.
fn continues(snap: &GridSnapshot, r: u16) -> Option<Wrap> {
    continues_within(snap, r, STITCH_MAX)
}

/// [`continues`], looking back at most `depth` rows for a run of pieces.
fn continues_within(snap: &GridSnapshot, r: u16, depth: u16) -> Option<Wrap> {
    let rows = (snap.cells.len() / snap.cols.max(1) as usize) as u16;
    if r + 1 >= rows {
        return None;
    }
    let this = row_chars(snap, r);
    if this.last().is_some_and(|c| c.c != ' ') {
        return Some(Wrap::Soft);
    }
    let text: String = this.iter().map(|c| c.c).collect();
    let last = text.trim_end().rsplit(' ').next().unwrap_or("");
    // A row holding nothing but one URL-character token is a middle piece of
    // a long URL even with no `/` or `.` in it: an OAuth link wrapped into
    // `…&code_challenge=8Ab1…` rows was cut short there (2026-10-14). Only
    // when the row above runs into it and both end at the same column (the
    // program's wrap width); a shorter row is the URL's tail, and gluing the
    // next row's `(expires …` onto it would open the wrong address. A tail
    // that happens to end exactly at the wrap width still looks like a piece;
    // the text cannot tell them apart.
    let end_col = |t: &str| t.trim_end().chars().count();
    let sole_piece = last == text.trim() && depth > 0 && r > 0 && {
        let above: String = row_chars(snap, r - 1).iter().map(|c| c.c).collect();
        end_col(&above) == end_col(&text)
            && continues_within(snap, r - 1, depth - 1) == Some(Wrap::Hard)
    };
    let ends_mid_url = !last.is_empty()
        && last.chars().all(is_url_char)
        && !last.ends_with(['.', ',', ';', ':', ')', ']', '!', '?', '\''])
        && (last.contains("://") || last.contains('/') || last.contains('.') || sole_piece);
    if !ends_mid_url {
        return None;
    }
    let next: String = row_chars(snap, r + 1).iter().map(|c| c.c).collect();
    let first = next.trim_start().split(' ').next().unwrap_or("");
    // A list bullet is the start of a new item, never the rest of a URL:
    // "…?item=289" over "- #259 club rides" opened `…item=289-` (2026-10-03).
    // Numbered: 1–3 digits + `.`/`)` with the item's text after it — so a
    // wrapped `…p179026479900` + `6399)` is still a continuation.
    let numbered = first.is_ascii()
        && (2..=4).contains(&first.len())
        && first[..first.len() - 1].chars().all(|c| c.is_ascii_digit())
        && first.ends_with(['.', ')'])
        && !next.trim_start()[first.len()..].trim().is_empty();
    let bullet = matches!(first, "-" | "*" | "+" | "•") || numbered;
    let continuation = !first.is_empty()
        && !bullet
        && next.starts_with(' ') == indented(&text)
        && first.chars().all(is_url_char)
        && first.chars().any(|c| c.is_ascii_alphanumeric())
        && !first.chars().all(|c| c.is_ascii_alphabetic())
        // One URL per row in an unbulleted list: the next is its own link.
        && !first.starts_with("http://")
        && !first.starts_with("https://");
    continuation.then_some(Wrap::Hard)
}

/// Does the row's text sit in from the left edge? Claude's reply marker
/// (`● ` / `⏺ `) or a bullet counts as indent: the reply's first row starts
/// at column 0 while its wrapped rows start with spaces.
fn indented(text: &str) -> bool {
    let mut cs = text.chars();
    match (cs.next(), cs.next()) {
        (Some(' '), _) => true,
        (Some(c), Some(' ')) => !c.is_alphanumeric(),
        _ => false,
    }
}

/// The wrapped run of rows containing `row`, as one string, plus where `col`
/// landed in it. A phone terminal is ~45 columns wide, so any URL worth
/// tapping is wrapped — resolving one row alone hands back a fragment that
/// opens nothing.
fn logical_line(snap: &GridSnapshot, row: u16, col: u16) -> Option<(String, usize)> {
    let cols = snap.cols as usize;
    if cols == 0 || snap.cells.is_empty() {
        return None;
    }
    let rows = (snap.cells.len() / cols) as u16;
    if row >= rows {
        return None;
    }
    let mut start = row;
    while start > 0 && row - start < STITCH_MAX && continues(snap, start - 1).is_some() {
        start -= 1;
    }
    let mut end = row;
    while end - row < STITCH_MAX && continues(snap, end).is_some() {
        end += 1;
    }

    let mut line: Vec<char> = Vec::with_capacity(cols * (end - start + 1) as usize);
    let mut hit = 0;
    let mut joined_hard = false;
    for r in start..=end {
        let mut cells: Vec<char> = row_chars(snap, r).iter().map(|c| c.c).collect();
        let mut lead = 0;
        if joined_hard {
            lead = cells.iter().take_while(|c| **c == ' ').count();
            cells.drain(..lead);
        }
        let hard = continues(snap, r) == Some(Wrap::Hard) && r < end;
        if hard {
            while cells.last() == Some(&' ') {
                cells.pop();
            }
        }
        if r == row {
            hit = line.len() + (col as usize).saturating_sub(lead);
        }
        line.extend(cells);
        joined_hard = hard;
    }
    Some((line.into_iter().collect(), hit))
}

/// The grid's own default colours: the most common (fg, bg) of its blank
/// cells. A live grid never holds the 0xFFFFFFFF sentinel for plain text:
/// every session seeds OSC 10/11 at spawn, so "default" arrives as the
/// palette's RGB. Erased cells carry it, and most blanks are erased cells.
pub fn plain_style(snap: &GridSnapshot) -> (u32, u32) {
    let mut counts: std::collections::HashMap<(u32, u32), usize> = Default::default();
    for c in snap.cells.iter().filter(|c| c.c == ' ') {
        *counts.entry((c.fg, c.bg)).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map(|(k, _)| k)
        .unwrap_or((DEFAULT_COLOR, DEFAULT_COLOR))
}

/// The inline code span under one cell: what a double-click copies whole.
///
/// Claude draws `` `! sudo …` `` in its own text colour with the backticks
/// dropped, so the grid's only trace of the span is its style. Word selection
/// stopped at the first space or `=` and copied half a command. The span is
/// the cells around `col` sharing its exact style, followed across rows when
/// Claude wrapped it.
///
/// Only a coloured run on the plain background counts, and only when it sits
/// in other text: on one row, something beside it; across rows, text before
/// its start and after its end. A wholly coloured line or block (a diff row,
/// an error, a dim tool result) keeps word selection.
pub fn code_span_at(snap: &GridSnapshot, row: u16, col: u16) -> Option<String> {
    let cols = snap.cols as usize;
    if cols == 0 || snap.cells.is_empty() {
        return None;
    }
    let rows = (snap.cells.len() / cols) as u16;
    if row >= rows || col as usize >= cols {
        return None;
    }
    let (plain_fg, plain_bg) = plain_style(snap);
    let hit = row_chars(snap, row)[col as usize];
    if hit.fg == plain_fg || hit.bg != plain_bg || hit.inverse {
        return None;
    }
    let same = |c: &CellSnap| {
        (c.fg, c.bg, c.bold, c.dim, c.italic, c.underline, c.inverse)
            == (
                hit.fg,
                hit.bg,
                hit.bold,
                hit.dim,
                hit.italic,
                hit.underline,
                hit.inverse,
            )
    };
    let blank = |cs: &[CellSnap]| cs.iter().all(|c| c.c == ' ');

    let cells = row_chars(snap, row);
    let (mut a, mut b) = (col as usize, col as usize);
    while a > 0 && same(&cells[a - 1]) {
        a -= 1;
    }
    while b + 1 < cols && same(&cells[b + 1]) {
        b += 1;
    }
    // (row, first col, last col) of each piece, top to bottom.
    let mut pieces = vec![(row, a, b)];

    // Wrapped: the span ends its row and the next row's text opens with it.
    while pieces.len() < STITCH_MAX as usize {
        let (r, _, end) = *pieces.last().unwrap();
        if r + 1 >= rows || !blank(&row_chars(snap, r)[end + 1..]) {
            break;
        }
        let next = row_chars(snap, r + 1);
        let Some(s) = next.iter().position(|c| c.c != ' ') else {
            break;
        };
        if !same(&next[s]) {
            break;
        }
        let mut e = s;
        while e + 1 < cols && same(&next[e + 1]) {
            e += 1;
        }
        pieces.push((r + 1, s, e));
    }
    while pieces.len() < STITCH_MAX as usize {
        let (r, start, _) = pieces[0];
        if r == 0 || !blank(&row_chars(snap, r)[..start]) {
            break;
        }
        let prev = row_chars(snap, r - 1);
        let Some(e) = prev.iter().rposition(|c| c.c != ' ') else {
            break;
        };
        if !same(&prev[e]) {
            break;
        }
        let mut s = e;
        while s > 0 && same(&prev[s - 1]) {
            s -= 1;
        }
        pieces.insert(0, (r - 1, s, e));
    }

    let before = |&(r, s, _): &(u16, usize, usize)| !blank(&row_chars(snap, r)[..s]);
    let after = |&(r, _, e): &(u16, usize, usize)| !blank(&row_chars(snap, r)[e + 1..]);
    let (first, last) = (&pieces[0], &pieces[pieces.len() - 1]);
    let set_apart = if pieces.len() == 1 {
        before(first) || after(first)
    } else {
        before(first) && after(last)
    };
    if !set_apart {
        return None;
    }
    let text = pieces
        .iter()
        .map(|&(r, s, e)| {
            let t: String = row_chars(snap, r)[s..=e].iter().map(|c| c.c).collect();
            t.trim().to_string()
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(text).filter(|t| !t.is_empty())
}

/// The http(s) URL in `line` whose character range covers `col`.
///
/// Indexes by CHAR, not byte: a row of terminal cells is one char per column,
/// so a non-ASCII glyph anywhere left of the URL would slide every byte offset
/// off by one.
pub fn http_url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let starts_at = |i: usize| {
        chars[i..].starts_with(&['h', 't', 't', 'p', 's', ':', '/', '/'])
            || chars[i..].starts_with(&['h', 't', 't', 'p', ':', '/', '/'])
    };
    let mut i = 0;
    while i < chars.len() {
        if !starts_at(i) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_url_char(chars[i]) {
            i += 1;
        }
        let url: String = chars[start..i].iter().collect();
        let url = url.trim_end_matches(['.', ',', ')', ']', ';', ':']);
        if url.len() > 10 && col >= start && col < start + url.chars().count() {
            return Some(url.to_string());
        }
    }
    None
}

/// How far a path may run across spaces from where it starts, in chars.
const PATH_MAX: usize = 400;

/// Rows a path may be followed across either side of the hit row.
const PATH_ROWS: u16 = 6;

/// File paths a double-click on `(row, col)` could mean, longest first.
///
/// Agents print paths like `/home/you/Projects/My Demo/LOG.md`
/// or `~/sites/demo/` in prose, wrapped across rows and with spaces in them,
/// so the grid can't say where one ends. Every absolute or `~/` path that
/// starts at a word boundary and runs over the hit is offered, ending at each
/// later space or at the line's end; the caller keeps the first that exists.
/// Trailing punctuation and a `:line:col` suffix are dropped.
pub fn path_candidates_at(snap: &GridSnapshot, row: u16, col: u16) -> Vec<String> {
    let cols = snap.cols as usize;
    if cols == 0 || col as usize >= cols || (row as usize + 1) * cols > snap.cells.len() {
        return Vec::new();
    }
    if row_chars(snap, row)[col as usize].c == ' ' {
        return Vec::new();
    }
    // Claude wraps prose at a space, but breaks a token too long for its
    // width mid-word: read each of its wraps both ways.
    let mut out: Vec<String> = Vec::new();
    for glue in [false, true] {
        if let Some((chars, hit)) = path_line(snap, row, col, glue) {
            for p in paths_over(&chars, hit) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.chars().count()));
    out
}

/// Every path in `chars` that starts at a word boundary and covers `hit`.
fn paths_over(chars: &[char], hit: usize) -> Vec<String> {
    let boundary = |i: usize| i == 0 || " \t`'\"([<=".contains(chars[i - 1]);
    let starts_path = |i: usize| {
        boundary(i)
            && ((chars[i] == '/' && chars.get(i + 1).is_some_and(|c| *c != ' ' && *c != '/'))
                || (chars[i] == '~' && chars.get(i + 1) == Some(&'/')))
    };
    let mut out: Vec<String> = Vec::new();
    for s in (hit.saturating_sub(PATH_MAX)..=hit).rev() {
        if !starts_path(s) {
            continue;
        }
        let limit = chars.len().min(s + PATH_MAX);
        let mut ends: Vec<usize> = (hit + 1..limit).filter(|&e| chars[e] == ' ').collect();
        ends.push(limit);
        for e in ends {
            let raw: String = chars[s..e].iter().collect();
            let p = trim_path(&raw);
            if p.chars().count() > 1 && s + p.chars().count() > hit && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// The rows around `row` joined the way a path could run through them, plus
/// where `col` landed. A full row runs straight on (the terminal wrapped it).
/// A shorter row followed by an indented one is Claude wrapping its reply:
/// the indent is dropped and the rows join with one space, or with none
/// when `glue` (a token Claude broke mid-word).
fn path_line(snap: &GridSnapshot, row: u16, col: u16, glue: bool) -> Option<(Vec<char>, usize)> {
    let cols = snap.cols as usize;
    if cols == 0 || snap.cells.is_empty() || col as usize >= cols {
        return None;
    }
    let rows = (snap.cells.len() / cols) as u16;
    if row >= rows {
        return None;
    }
    let text = |r: u16| -> Vec<char> { row_chars(snap, r).iter().map(|c| c.c).collect() };
    // How row `r + 1` joins onto row `r`: Some(true) runs on, Some(false)
    // adds a space, None ends the line.
    let joins = |r: u16| -> Option<bool> {
        if r + 1 >= rows {
            return None;
        }
        let (this, next) = (text(r), text(r + 1));
        if next.iter().all(|c| *c == ' ') || this.iter().all(|c| *c == ' ') {
            return None;
        }
        if this.last() != Some(&' ') {
            Some(true)
        } else {
            (next[0] == ' ').then_some(false)
        }
    };
    let mut start = row;
    while start > 0 && row - start < PATH_ROWS && joins(start - 1).is_some() {
        start -= 1;
    }
    let mut line: Vec<char> = Vec::new();
    let mut hit = 0;
    let mut how: Option<bool> = None;
    for r in start..rows.min(row + PATH_ROWS + 1) {
        let mut cells = text(r);
        let mut lead = 0;
        if how == Some(false) {
            lead = cells.iter().take_while(|c| **c == ' ').count();
            cells.drain(..lead);
            while line.last() == Some(&' ') {
                line.pop();
            }
            if !glue {
                line.push(' ');
            }
        }
        if r == row {
            hit = line.len() + (col as usize).saturating_sub(lead);
        }
        line.extend(cells);
        how = joins(r);
        if how.is_none() {
            break;
        }
    }
    (hit < line.len()).then_some((line, hit))
}

/// A printed path minus what prose puts after it: `.`, `,`, `)`, quotes,
/// backticks and a `:12` or `:12:3` position.
fn trim_path(raw: &str) -> String {
    let mut p = raw
        .trim_end()
        .trim_end_matches(['.', ',', ';', ':', ')', ']', '\'', '"', '`', '>']);
    for _ in 0..2 {
        if let Some((head, tail)) = p.rsplit_once(':') {
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                p = head;
            }
        }
    }
    p.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::HyperlinkSpan;

    /// A grid holding one line of text per row, padded to `cols`.
    fn grid_of(cols: usize, lines: &[&str]) -> GridSnapshot {
        let mut snap = GridSnapshot::empty("t-1");
        snap.cols = cols as u16;
        snap.rows = lines.len() as u16;
        snap.cells = Vec::with_capacity(cols * lines.len());
        for line in lines {
            let mut chars: Vec<char> = line.chars().collect();
            chars.resize(cols, ' ');
            for c in chars {
                let mut cell = CellSnap::blank();
                cell.c = c;
                snap.cells.push(cell);
            }
        }
        snap
    }

    /// The regression: ctrl+click used to ignore where you clicked and open
    /// the first link on screen, so clicking a plat URL under a PR reference
    /// opened the PR.
    #[test]
    fn a_click_resolves_the_url_under_the_cursor_not_the_first_on_screen() {
        let snap = grid_of(
            80,
            &[
                "see https://github.com/o/r/pull/6807 for the CI run",
                "plat https://cadence.ham.xyz/plats/86ff368c-92db for the numbers",
            ],
        );
        assert_eq!(
            url_at_cell(&snap, 1, 10).as_deref(),
            Some("https://cadence.ham.xyz/plats/86ff368c-92db")
        );
        assert_eq!(
            url_at_cell(&snap, 0, 10).as_deref(),
            Some("https://github.com/o/r/pull/6807")
        );
    }

    /// Clicking off any link opens nothing — better than opening something
    /// arbitrary from elsewhere on screen.
    #[test]
    fn a_click_on_plain_text_opens_nothing() {
        let snap = grid_of(80, &["plat https://cadence.ham.xyz/x for the numbers"]);
        assert_eq!(url_at_cell(&snap, 0, 0), None);
        assert_eq!(url_at_cell(&snap, 0, 40), None);
    }

    /// An OSC-8 span wins over bare text, but only on the cells it covers.
    #[test]
    fn an_osc8_span_only_claims_its_own_cells() {
        let mut snap = grid_of(80, &["#6807 and https://cadence.ham.xyz/plats/abc"]);
        snap.hyperlinks.push(HyperlinkSpan {
            row: 0,
            col_start: 0,
            col_end: 5,
            uri: "https://github.com/o/r/pull/6807".into(),
        });
        assert_eq!(
            url_at_cell(&snap, 0, 2).as_deref(),
            Some("https://github.com/o/r/pull/6807")
        );
        assert_eq!(
            url_at_cell(&snap, 0, 20).as_deref(),
            Some("https://cadence.ham.xyz/plats/abc")
        );
    }

    /// Column is a CHAR offset, not a byte one — a wide glyph left of the URL
    /// would otherwise slide the hit box.
    #[test]
    fn a_non_ascii_glyph_does_not_shift_the_hit_box() {
        let snap = grid_of(80, &["✦ ✦ https://cadence.ham.xyz/plats/abc"]);
        assert_eq!(
            url_at_cell(&snap, 0, 4).as_deref(),
            Some("https://cadence.ham.xyz/plats/abc")
        );
        assert_eq!(url_at_cell(&snap, 0, 3), None);
    }

    /// The phone case: 44 columns wide, so the URL is in three pieces. A tap
    /// on ANY of them has to produce the whole thing.
    #[test]
    fn a_wrapped_url_is_stitched_back_together_from_any_row() {
        // 40 cols. Row 0 is full to the last column — the wrap signal — and
        // row 1 is not, which ends the logical line.
        let snap = grid_of(
            40,
            &[
                "opened https://github.com/example/repo/p",
                "ull/12345 and the CI is green",
            ],
        );
        let want = Some("https://github.com/example/repo/pull/12345");
        assert_eq!(url_at_cell(&snap, 0, 10).as_deref(), want);
        assert_eq!(url_at_cell(&snap, 1, 2).as_deref(), want);
    }

    /// Claude/ink wrap a long URL themselves — short of the right edge, with
    /// the rest indented — which the full-row rule never saw. Real frames
    /// from 2026-09-28; a tap on any piece must open the whole URL.
    #[test]
    fn a_program_wrapped_url_is_stitched_across_its_indent() {
        let snap = grid_of(
            48,
            &[
                "     prod. https://github.com/example/re",
                "     po/pull/4242",
            ],
        );
        let want = Some("https://github.com/example/repo/pull/4242");
        assert_eq!(url_at_cell(&snap, 0, 20).as_deref(), want);
        assert_eq!(url_at_cell(&snap, 1, 8).as_deref(), want);

        // Word-wrapped at a '/', far from the edge, prose after it.
        let snap = grid_of(
            50,
            &[
                "  v2 is live at https://vita-reports.ham.xyz",
                "  /s/675a4e6f8887. There are no more",
            ],
        );
        let want = Some("https://vita-reports.ham.xyz/s/675a4e6f8887");
        assert_eq!(url_at_cell(&snap, 0, 20).as_deref(), want);
        assert_eq!(url_at_cell(&snap, 1, 4).as_deref(), want);

        // Markdown hanging indent; the closing paren is not part of the URL.
        let snap = grid_of(
            80,
            &[
                "      41  - [thread](https://example.slack.com/archives/C0123456789/p1790264799",
                "          006399)",
            ],
        );
        assert_eq!(
            url_at_cell(&snap, 0, 30).as_deref(),
            Some("https://example.slack.com/archives/C0123456789/p1790264799006399")
        );
    }

    /// Real frames from 2026-10-14. A long OAuth link wraps into rows that
    /// hold only query string (no `/` or `.`), and runs past six rows; a URL
    /// on the first row of a Claude reply sits after the `●` marker while
    /// the rest is indented.
    #[test]
    fn long_and_reply_marker_urls_are_stitched_whole() {
        let snap = grid_of(
            62,
            &[
                "  1. Open this link and approve: https://www.figma.com/oauth",
                "     /mcp?response_type=code&client_id=7TMfnUEIBxoSzzOD5LzcT",
                "     i&code_challenge=8Ab1EFexbQD_im4i3VeuVCus4TOgmSM3xgnxGW",
                "     AieFc&code_challenge_method=S256&redirect_uri=http%3A%2",
                "     F%2Flocalhost%3A51344%2Fcallback&state=MI0goXog_g_bgwyx",
                "     _59lrNZs3Ce4SDolodhMFfyf8sg&scope=mcp%3Aconnect&resourc",
                "     e=https%3A%2F%2Fmcp.figma.com%2Fmcp",
                "  2. If the page that loads after you approve shows an",
            ],
        );
        let want = Some(
            "https://www.figma.com/oauth/mcp?response_type=code&client_id=7TMfnUEIBxoSzzOD5LzcT\
             i&code_challenge=8Ab1EFexbQD_im4i3VeuVCus4TOgmSM3xgnxGWAieFc&code_challenge_method=\
             S256&redirect_uri=http%3A%2F%2Flocalhost%3A51344%2Fcallback&state=MI0goXog_g_bgwyx\
             _59lrNZs3Ce4SDolodhMFfyf8sg&scope=mcp%3Aconnect&resource=https%3A%2F%2Fmcp.figma.com%2Fmcp",
        );
        for row in 0..7 {
            let col = if row == 0 { 40 } else { 8 };
            assert_eq!(url_at_cell(&snap, row, col).as_deref(), want, "row {row}");
        }
        assert_eq!(url_at_cell(&snap, 7, 8), None);

        let snap = grid_of(
            70,
            &[
                "● Filed it under the epic: https://github.com/example/re",
                "  po/issues/4243 and set the roadmap fields.",
            ],
        );
        let want = Some("https://github.com/example/repo/issues/4243");
        assert_eq!(url_at_cell(&snap, 0, 40).as_deref(), want);
        assert_eq!(url_at_cell(&snap, 1, 4).as_deref(), want);
    }

    /// Review cases (2026-10-14): a URL's short tail row does not swallow the
    /// prose under it, a URL list without bullets stays separate, a glyph
    /// opening the next row does not panic, and a phone-width OAuth link of
    /// 18 rows opens whole from any row.
    #[test]
    fn url_tails_lists_and_long_phone_urls() {
        let snap = grid_of(
            50,
            &[
                "  Open: https://acc.example.com/o?client=1",
                "  234&state=abcXYZ123",
                "  (expires in 10 minutes)",
            ],
        );
        let want = Some("https://acc.example.com/o?client=1234&state=abcXYZ123");
        assert_eq!(url_at_cell(&snap, 0, 12).as_deref(), want);

        let snap = grid_of(40, &["  https://a.com/x", "  https://b.com/y"]);
        assert_eq!(url_at_cell(&snap, 0, 4).as_deref(), Some("https://a.com/x"));

        let snap = grid_of(40, &["  see https://x.example.com/a/b", "  ⎿  done"]);
        assert_eq!(
            url_at_cell(&snap, 0, 8).as_deref(),
            Some("https://x.example.com/a/b")
        );

        let url: String = "https://auth.example.com/oauth/authorize?"
            .chars()
            .chain((0..600).map(|i| if i % 9 == 0 { '&' } else { 'a' }))
            .collect();
        let chars: Vec<char> = url.chars().collect();
        let rows: Vec<String> = chars
            .chunks(38)
            .map(|c| format!("  {}", c.iter().collect::<String>()))
            .collect();
        let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
        let snap = grid_of(45, &refs);
        for row in 0..refs.len() as u16 {
            assert_eq!(
                url_at_cell(&snap, row, 4).as_deref(),
                Some(url.as_str()),
                "row {row}"
            );
        }
    }

    /// A markdown list of URLs, one per line (2026-10-03): the next item's
    /// bullet is not the rest of this URL.
    #[test]
    fn a_list_bullet_on_the_next_line_is_not_part_of_the_url() {
        let snap = grid_of(
            80,
            &[
                "- #289 personal chat link: http://localhost:5390/#/board?item=289",
                "- #259 club rides: http://localhost:5390/#/board?item=259",
                "1. http://a.io/x?id=7",
                "2. http://b.io/y",
                "* http://c.io/z?q=1",
                "+ more",
            ],
        );
        assert_eq!(
            url_at_cell(&snap, 0, 40).as_deref(),
            Some("http://localhost:5390/#/board?item=289")
        );
        assert_eq!(
            url_at_cell(&snap, 1, 30).as_deref(),
            Some("http://localhost:5390/#/board?item=259")
        );
        assert_eq!(
            url_at_cell(&snap, 2, 6).as_deref(),
            Some("http://a.io/x?id=7")
        );
        assert_eq!(
            url_at_cell(&snap, 4, 4).as_deref(),
            Some("http://c.io/z?q=1")
        );
    }

    /// Never glue a finished URL onto the next line's prose.
    #[test]
    fn a_url_that_ends_its_line_is_not_glued_to_prose() {
        let snap = grid_of(
            50,
            &[
                "  see https://a.io/x",
                "  tra text here",
                "  https://b.io/y.",
                "  2 more",
            ],
        );
        assert_eq!(url_at_cell(&snap, 0, 8).as_deref(), Some("https://a.io/x"));
        // Sentence-final '.' closes it even though "2" looks like a continuation.
        assert_eq!(url_at_cell(&snap, 2, 5).as_deref(), Some("https://b.io/y"));
    }

    /// Stitching must not walk off into unrelated rows: a row that ends in a
    /// blank is a line break, so the URL stops there.
    #[test]
    fn a_line_ending_in_blank_does_not_swallow_the_next_row() {
        let snap = grid_of(20, &["see https://a.io/x", "tra text here"]);
        assert_eq!(url_at_cell(&snap, 0, 6).as_deref(), Some("https://a.io/x"));
    }

    const PLAIN_FG: u32 = 0x00D8_DEE9;
    const PLAIN_BG: u32 = 0x002E_3440;
    const SPAN_FG: u32 = 0x00B1_B9F9;

    /// [`grid_of`] with plain cells in resolved RGB, as a live session has
    /// them (OSC 10/11 are seeded at spawn, so no cell keeps the sentinel).
    fn live_grid(cols: usize, lines: &[&str]) -> GridSnapshot {
        let mut snap = grid_of(cols, lines);
        for c in &mut snap.cells {
            c.fg = PLAIN_FG;
            c.bg = PLAIN_BG;
        }
        snap
    }

    /// Colour `len` cells of `row` from `col`, the way Claude paints a code span.
    fn paint(snap: &mut GridSnapshot, row: usize, col: usize, len: usize, fg: u32) {
        let cols = snap.cols as usize;
        for c in &mut snap.cells[row * cols + col..row * cols + col + len] {
            c.fg = fg;
        }
    }

    /// A double-click anywhere in `! sudo pacman -Syu`, the space included,
    /// copies all of it, not the word `sudo` (2026-10-23).
    #[test]
    fn a_double_click_in_a_code_span_takes_the_whole_span() {
        let mut snap = live_grid(60, &["run ! sudo pacman -Syu in the pane"]);
        paint(&mut snap, 0, 4, 18, SPAN_FG);
        let want = Some("! sudo pacman -Syu");
        assert_eq!(code_span_at(&snap, 0, 4).as_deref(), want);
        assert_eq!(code_span_at(&snap, 0, 10).as_deref(), want);
        assert_eq!(code_span_at(&snap, 0, 12).as_deref(), want);
        assert_eq!(code_span_at(&snap, 0, 0), None);
        assert_eq!(code_span_at(&snap, 0, 26), None);
    }

    /// Plain prose around a span keeps word selection: plain text is the
    /// grid's resolved default colour, not the sentinel, and bold is still
    /// that colour.
    #[test]
    fn plain_and_bold_words_beside_a_span_are_not_spans() {
        let mut snap = live_grid(
            40,
            &[
                "● Then run cargo build and",
                "  check the log before you go on.",
            ],
        );
        paint(&mut snap, 0, 11, 11, SPAN_FG);
        snap.cells[40 + 2..40 + 7]
            .iter_mut()
            .for_each(|c| c.bold = true);
        assert_eq!(code_span_at(&snap, 0, 13).as_deref(), Some("cargo build"));
        assert_eq!(code_span_at(&snap, 0, 3), None);
        assert_eq!(code_span_at(&snap, 0, 24), None);
        assert_eq!(code_span_at(&snap, 1, 3), None);
        assert_eq!(code_span_at(&snap, 1, 12), None);
    }

    /// A coloured line with nothing else on it is a diff row or a dim tool
    /// result, not a span: double-click there still selects a word. Same for
    /// text on a coloured background.
    #[test]
    fn a_wholly_coloured_line_is_not_a_span() {
        let mut snap = live_grid(40, &["  + let x = compute(y);", "  - let x = 1;"]);
        paint(&mut snap, 0, 2, 21, 0x0000_FF00);
        assert_eq!(code_span_at(&snap, 0, 6), None);
        snap.cells[40 + 2..40 + 14]
            .iter_mut()
            .for_each(|c| c.bg = 0x0040_0000);
        paint(&mut snap, 1, 4, 3, SPAN_FG);
        assert_eq!(code_span_at(&snap, 1, 5), None);
    }

    /// A red error block whose prefix sits on its first row only is a block,
    /// not a wrapped span: its end has nothing after it.
    #[test]
    fn a_coloured_block_under_a_prefix_is_not_a_span() {
        let mut snap = live_grid(
            40,
            &[
                "  ⎿  error: could not compile",
                "     due to 2 previous errors",
            ],
        );
        paint(&mut snap, 0, 5, 24, 0x00FF_5F6D);
        paint(&mut snap, 1, 5, 24, 0x00FF_5F6D);
        assert_eq!(code_span_at(&snap, 1, 8), None);
    }

    /// Claude wraps a long command onto an indented row; a click on either
    /// piece copies the whole command.
    #[test]
    fn a_wrapped_code_span_is_joined_from_either_row() {
        let mut snap = live_grid(
            40,
            &[
                "● Then run sudo systemctl restart",
                "  seance-daemon.service on the server.",
            ],
        );
        paint(&mut snap, 0, 11, 22, SPAN_FG);
        paint(&mut snap, 1, 2, 21, SPAN_FG);
        let want = Some("sudo systemctl restart seance-daemon.service");
        assert_eq!(code_span_at(&snap, 0, 17).as_deref(), want);
        assert_eq!(code_span_at(&snap, 1, 5).as_deref(), want);
    }

    /// Double-click a path, spaces, ü and Claude's word wrap included: every
    /// reading is offered, longest first, so the GUI can keep the one that
    /// exists (2026-10-30).
    #[test]
    fn a_path_is_offered_at_every_space_longest_first() {
        let snap = grid_of(
            44,
            &[
                "● Saved to /home/you/Projects/Seance",
                "  Demo/Demo log.md. Next I run it.",
                "",
                "  /tmp/elsewhere",
            ],
        );
        let got = path_candidates_at(&snap, 0, 14);
        assert_eq!(
            got.first().map(String::as_str),
            Some("/home/you/Projects/Seance Demo/Demo log.md. Next I run it")
        );
        assert!(got
            .iter()
            .any(|p| p == "/home/you/Projects/Seance Demo/Demo log.md"));
        assert!(got.iter().any(|p| p == "/home/you/Projects/Seance"));
        assert!(!got.iter().any(|p| p.contains("elsewhere")));
        // A click on "Demo" (the wrapped row) only offers readings that reach
        // it, whether the wrap was a space or a token broken mid-word.
        let got = path_candidates_at(&snap, 1, 4);
        assert!(got.iter().all(|p| {
            p.starts_with("/home/you/Projects/Seance Demo")
                || p.starts_with("/home/you/Projects/SeanceDemo")
        }));
        assert!(got
            .iter()
            .any(|p| p == "/home/you/Projects/Seance Demo/Demo log.md"));
    }

    /// A long path the terminal broke mid-word runs straight on.
    #[test]
    fn a_path_cut_at_the_right_edge_runs_on() {
        let snap = grid_of(20, &["at /var/log/seance-d", "aemon.log now"]);
        let got = path_candidates_at(&snap, 1, 2);
        assert!(got.iter().any(|p| p == "/var/log/seance-daemon.log"));
    }

    /// A double-click on blank cells (a wrapped row's empty tail or the next
    /// row's indent) is never a path, from review (2026-10-30).
    #[test]
    fn blank_cells_beside_a_wrapped_path_are_not_paths() {
        let snap = grid_of(
            44,
            &[
                "● Saved to /home/you/Projects/Seance",
                "  Demo/Demo log.md. Next I run it.",
            ],
        );
        assert!(path_candidates_at(&snap, 0, 42).is_empty());
        assert!(path_candidates_at(&snap, 1, 0).is_empty());
    }

    /// Claude breaks a path longer than its width mid-word and indents the
    /// rest; a terminal wrap keeps the next row's leading space.
    #[test]
    fn a_path_broken_mid_word_or_wrapped_before_a_space_is_found() {
        let snap = grid_of(
            30,
            &["● see /home/you/Projects/ve  ", "  ry-long/LOG.md now"],
        );
        let got = path_candidates_at(&snap, 0, 8);
        assert!(got
            .iter()
            .any(|p| p == "/home/you/Projects/very-long/LOG.md"));
        let snap = grid_of(17, &["/tmp/My Folder/My", " Notes/x.md"]);
        let got = path_candidates_at(&snap, 0, 2);
        assert!(got.iter().any(|p| p == "/tmp/My Folder/My Notes/x.md"));
    }

    #[test]
    fn a_home_path_drops_its_line_number_and_quotes() {
        let snap = grid_of(50, &["see `~/sites/bin/publish:12:3`, then go"]);
        let got = path_candidates_at(&snap, 0, 8);
        assert!(got.iter().any(|p| p == "~/sites/bin/publish"));
    }

    #[test]
    fn words_and_slashes_inside_words_are_not_paths() {
        let snap = grid_of(50, &["read and/or write a 1/2 cup, then // done"]);
        assert!(path_candidates_at(&snap, 0, 2).is_empty());
        assert!(path_candidates_at(&snap, 0, 9).is_empty());
        assert!(path_candidates_at(&snap, 0, 22).is_empty());
        assert!(path_candidates_at(&snap, 0, 35).is_empty());
    }
}
