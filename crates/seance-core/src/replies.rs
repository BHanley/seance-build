//! Which rows of a grid are Claude Code's own reply, so the GUI can paint
//! them apart from tool output ("so I can tell what is important to read vs
//! terminal output").
//!
//! Claude Code draws its prose and a command's output in the same plain
//! colour, so only the layout tells them apart: a reply opens with `● ` at
//! column 0 in Claude's text colour, while a tool call's dot is green, red or
//! dim. Everything below it, up to the next row that starts at column 0, is
//! that reply.

use crate::links::plain_style;
use crate::snapshot::{CellSnap, GridSnapshot};

/// How far, per channel, a reply dot's colour may sit from the plain text
/// colour. Claude may send the 256-colour cube's nearest match instead of
/// the exact RGB; a tool's dim dot is further off than this.
const DOT_SLACK: u32 = 48;

/// One flag per row: true for the rows of a Claude reply, plus the plain
/// (fg, bg) the flags were judged against. Empty when no reply dot is on screen.
pub fn claude_reply_rows(snap: &GridSnapshot) -> (Vec<bool>, (u32, u32)) {
    let cols = snap.cols as usize;
    if cols < 3 || snap.cells.is_empty() {
        return (Vec::new(), (0, 0));
    }
    let rows = snap.cells.len() / cols;
    let row_at = |r: usize| &snap.cells[r * cols..(r + 1) * cols];
    if !(0..rows).any(|r| matches!(row_at(r)[0].c, '●' | '⏺')) {
        return (Vec::new(), (0, 0));
    }
    let (plain_fg, plain_bg) = plain_style(snap);
    // `Bash(…)`: a tool's name is bold and runs straight into its `(`.
    let tool_header = |row: &[CellSnap]| {
        let name = row[2..].iter().take_while(|c| c.bold && c.c != ' ').count();
        name > 0 && row.get(2 + name).is_some_and(|c| c.c == '(')
    };
    let mut out = vec![false; rows];
    // Rows above the first column-0 row belong to something that scrolled
    // off. A reply sits two columns in; tool output sits deeper or under `⎿`.
    let top = (0..rows).find(|&r| row_at(r)[0].c != ' ').unwrap_or(rows);
    let top_text: Vec<&[CellSnap]> = (0..top)
        .map(row_at)
        .filter(|row| row.iter().any(|c| c.c != ' '))
        .collect();
    let mut in_reply = !top_text.is_empty()
        && top_text
            .iter()
            .all(|row| row[2].c != '⎿' && !tool_header(row))
        && top_text
            .iter()
            .map(|row| row.iter().take_while(|c| c.c == ' ').count())
            .min()
            == Some(2);
    for (r, flag) in out.iter_mut().enumerate() {
        let row = row_at(r);
        if row[0].c != ' ' {
            in_reply = matches!(row[0].c, '●' | '⏺')
                && row[1].c == ' '
                && !row[0].dim
                && near(row[0].fg, plain_fg)
                && !tool_header(row);
        } else if row[1].c == ' ' && (row[2].c == '⎿' || tool_header(row)) {
            // A running tool's dot blinks to a space every 600 ms.
            in_reply = false;
        }
        *flag = in_reply;
    }
    (out, (plain_fg, plain_bg))
}

fn near(a: u32, b: u32) -> bool {
    if a == b {
        return true;
    }
    if a == 0xFFFF_FFFF || b == 0xFFFF_FFFF {
        return false;
    }
    (0..3).all(|i| {
        let (x, y) = ((a >> (i * 8)) & 0xff, (b >> (i * 8)) & 0xff);
        x.abs_diff(y) <= DOT_SLACK
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: u32 = 0x00E3_E7EE;
    const BG: u32 = 0x0030_353F;

    /// Rows as Claude Code lays them out, every cell in the plain colours.
    fn grid(cols: usize, lines: &[&str]) -> GridSnapshot {
        let mut snap = GridSnapshot::empty("t-1");
        snap.cols = cols as u16;
        snap.rows = lines.len() as u16;
        for line in lines {
            let mut chars: Vec<char> = line.chars().collect();
            chars.resize(cols, ' ');
            for c in chars {
                let mut cell = CellSnap::blank();
                cell.c = c;
                cell.fg = FG;
                cell.bg = BG;
                snap.cells.push(cell);
            }
        }
        snap
    }

    fn dot(snap: &mut GridSnapshot, row: usize, fg: u32) {
        let cols = snap.cols as usize;
        snap.cells[row * cols].fg = fg;
    }

    #[test]
    fn a_reply_runs_to_the_next_row_at_column_zero() {
        let mut snap = grid(
            30,
            &[
                "  tail of something above",
                "❯ fix the build",
                "● Bash(cargo build)",
                "  ⎿  error: missing ;",
                "● Fixed it: the semicolon was",
                "  missing in main.rs.",
                "",
                "  - Build passes now.",
                "✻ Brewed for 11s",
            ],
        );
        dot(&mut snap, 2, 0x0078_D787); // a tool's green dot
        dot(&mut snap, 4, 0x00FF_FFFF); // Claude's text colour, near the plain fg
        let (got, _) = claude_reply_rows(&snap);
        assert_eq!(
            got,
            // Row 0 is the two-column tail of an earlier reply.
            vec![true, false, false, false, true, true, true, true, false]
        );
    }

    #[test]
    fn a_dim_or_grey_dot_is_a_running_tool() {
        let mut snap = grid(30, &["● Read(src/main.rs)", "  ⎿  Read 40 lines"]);
        dot(&mut snap, 0, 0x0099_9999);
        assert_eq!(claude_reply_rows(&snap).0, vec![false, false]);
        dot(&mut snap, 0, FG);
        snap.cells[0].dim = true;
        assert_eq!(claude_reply_rows(&snap).0, vec![false, false]);
    }

    /// From review (2026-10-30): a running tool's blinking dot, a long reply
    /// whose dot scrolled off, and tool output whose header scrolled off.
    #[test]
    fn a_blinking_tool_dot_ends_the_reply() {
        let mut snap = grid(
            30,
            &[
                "● Let me check the logs.",
                "  Bash(tail -5 app.log)",
                "  ⎿  Running…",
            ],
        );
        dot(&mut snap, 0, FG);
        for c in &mut snap.cells[30 + 2..30 + 6] {
            c.bold = true;
        }
        assert_eq!(claude_reply_rows(&snap).0, vec![true, false, false]);
    }

    #[test]
    fn the_top_of_the_screen_is_a_reply_only_two_columns_in() {
        let mut snap = grid(
            30,
            &["  and the rest of the", "  reply goes on.", "● Done.", "❯ "],
        );
        dot(&mut snap, 2, FG);
        assert_eq!(claude_reply_rows(&snap).0, vec![true, true, true, false]);
        let mut snap = grid(30, &["     line 41 of output", "     line 42", "● Done."]);
        dot(&mut snap, 2, FG);
        assert_eq!(claude_reply_rows(&snap).0, vec![false, false, true]);
    }

    #[test]
    fn a_screen_with_no_dot_has_no_reply() {
        let snap = grid(30, &["  just some text", "$ ls"]);
        assert!(claude_reply_rows(&snap).0.is_empty());
    }
}
