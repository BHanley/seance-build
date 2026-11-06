//! Clickable task boxes for markdown file panes: a pure model over the SOURCE.
//!
//! `gpui_component`'s TextView draws `- [ ]` boxes but gives no click hook, and
//! Seance does not fork it. So, like [`crate::mdfold`], the work happens around
//! the renderer: before render, every task line gets an invisible marker
//! carrying its line number in the FILE (`<!--seance-task:N-->`); `fileview.rs`
//! swaps any list holding a marked task for a list it draws itself, with a
//! clickable box per task.
//!
//! The marker is what makes the click safe. Folding drops lines and the
//! frontmatter box swallows some, so a position in the rendered document says
//! nothing about where a line sits on disk. The marker is stamped on the
//! unfolded source, so it survives both, and the click writes by file line
//! number plus an exact-text check, never by render index.

/// Opens a task marker; the file line number and [`MARK_CLOSE`] follow.
pub const MARK_OPEN: &str = "<!--seance-task:";
pub const MARK_CLOSE: &str = "-->";

/// A task-list item line: the byte offset of the box character (the space or
/// `x` between the brackets) and whether it's checked. `None` for anything else.
///
/// Matches `- [ ] text`, `* [x] text`, `+ [X] text`, `1. [ ] text`,
/// `1) [ ] text`, at any indent. The box needs whitespace and some text after
/// it; an empty `- [ ]` is not a task in GFM either.
pub fn task_box(line: &str) -> Option<(usize, bool)> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let bullet = if rest.starts_with(['-', '*', '+']) {
        1
    } else {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if !(1..=9).contains(&digits) || !rest[digits..].starts_with(['.', ')']) {
            return None;
        }
        digits + 1
    };
    let after = &rest[bullet..];
    let gap = after.len() - after.trim_start_matches([' ', '\t']).len();
    if gap == 0 {
        return None;
    }
    let b = &after[gap..];
    let bytes = b.as_bytes();
    if bytes.len() < 4 || bytes[0] != b'[' || bytes[2] != b']' || !matches!(bytes[3], b' ' | b'\t')
    {
        return None;
    }
    let checked = match bytes[1] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    if b[4..].trim().is_empty() {
        return None;
    }
    Some((indent + bullet + gap + 1, checked))
}

/// Stamp every task line in `body` with its file line number, counting from
/// `first_line` (the frontmatter box hides the lines above the body).
///
/// Task items come from the same parser the renderer uses, so a `- [ ]` in a
/// code block or an HTML comment is never stamped. The marker goes where the
/// item's text starts, straight after `[ ] `, inside its first paragraph. An
/// item whose line doesn't read as a plain task line (a blockquote, a BOM)
/// stays unstamped and draws as a box that can't be clicked.
pub fn annotate(body: &str, first_line: usize) -> String {
    let Ok(tree) = markdown::to_mdast(body, &markdown::ParseOptions::gfm()) else {
        return body.to_string();
    };
    let starts: Vec<usize> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut at: Vec<(usize, usize)> = Vec::new(); // (byte offset, line)
    collect_tasks(&tree, body, &starts, &mut at);
    at.sort_unstable();
    let mut out = String::with_capacity(body.len() + at.len() * 24);
    let mut done = 0;
    for (offset, line) in at {
        out.push_str(&body[done..offset]);
        out.push_str(&format!("{MARK_OPEN}{}{MARK_CLOSE}", first_line + line));
        done = offset;
    }
    out.push_str(&body[done..]);
    out
}

fn collect_tasks(
    node: &markdown::mdast::Node,
    body: &str,
    starts: &[usize],
    at: &mut Vec<(usize, usize)>,
) {
    if let markdown::mdast::Node::ListItem(item) = node {
        if let (Some(_), Some(pos)) = (item.checked, item.position.as_ref()) {
            let line = pos.start.line.saturating_sub(1);
            let text_at = item
                .children
                .first()
                .and_then(|c| c.position())
                .map(|p| p.start.offset);
            if let (Some(&start), Some(text_at)) = (starts.get(line), text_at) {
                let end = starts.get(line + 1).copied().unwrap_or(body.len());
                // Only where our own line scan agrees: that's what flip() edits.
                if task_box(&body[start..end]).is_some_and(|(b, _)| start + b + 3 == text_at) {
                    at.push((text_at, line));
                }
            }
        }
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_tasks(child, body, starts, at);
        }
    }
}

/// The file line number in a line's marker, and the line with it removed.
pub fn take_marker(line: &str) -> Option<(usize, String)> {
    let start = line.find(MARK_OPEN)?;
    let num_at = start + MARK_OPEN.len();
    let len = line[num_at..].find(MARK_CLOSE)?;
    let n = line[num_at..num_at + len].parse().ok()?;
    let mut clean = String::with_capacity(line.len());
    clean.push_str(&line[..start]);
    clean.push_str(&line[num_at + len + MARK_CLOSE.len()..]);
    Some((n, clean))
}

/// Drop every marker from `text` (what a copy or a nested render should see).
pub fn strip_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(MARK_OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + MARK_OPEN.len()..];
        match after.find(MARK_CLOSE) {
            Some(end) => rest = &after[end + MARK_CLOSE.len()..],
            None => {
                rest = &rest[start..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A clickable box: where it lives on disk and what that line must still say.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskMark {
    /// 0-based line in the FILE.
    pub line: usize,
    pub checked: bool,
    /// The line as rendered, from its bullet on, marker removed. A click only
    /// writes when the file's line still reads exactly this (indent aside).
    pub expected: String,
}

/// One list item, split into the box (if it has one) and the markdown to
/// render beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskItem {
    pub mark: Option<TaskMark>,
    /// The parser saw a box here. Without a mark it draws but can't be clicked.
    pub checked: Option<bool>,
    pub body: String,
}

/// Split one list item's source into its box and body.
///
/// `src` starts at the item's bullet; `col` is the bullet's 0-based column in
/// its line (continuation lines in `src` carry their full indent, and lose up
/// to `col` + the bullet width of it, so nested lists keep their relative
/// shape in the body). `checked` is the parser's reading of the item's box.
pub fn split_item(src: &str, col: usize, checked: Option<bool>) -> TaskItem {
    let mut lines = src.split('\n');
    let first = lines.next().unwrap_or("").trim_end_matches('\r');
    let (line_no, first_clean) = match take_marker(first) {
        Some((n, clean)) => (Some(n), clean),
        None => (None, first.to_string()),
    };

    // An indented item's source can start at the line, not the bullet, so
    // measure that lead in columns (tabs to 4, as `dedent` counts them).
    let lead = first_clean.len() - first_clean.trim_start_matches([' ', '\t']).len();
    let lead_cols =
        first_clean[..lead]
            .chars()
            .fold(0, |c, ch| if ch == '\t' { (c / 4 + 1) * 4 } else { c + 1 });
    let bullet = bullet_width(&first_clean[lead..]);
    // Continuation lines indent to the item's content column, which is the
    // bullet's width even for a task (the box is part of the first paragraph).
    let content_at = col + lead_cols + bullet;
    let (head, mark) = match (line_no, task_box(&first_clean)) {
        (Some(line), Some((at, checked))) => (
            at + 3,
            Some(TaskMark {
                line,
                checked,
                expected: first_clean.clone(),
            }),
        ),
        _ => {
            let bullet = lead + bullet;
            let rest = first_clean.get(bullet..).unwrap_or("").as_bytes();
            let boxed = checked.is_some()
                && rest.len() >= 4
                && rest[0] == b'['
                && rest[2] == b']'
                && matches!(rest[3], b' ' | b'\t');
            (if boxed { bullet + 4 } else { bullet }, None)
        }
    };

    let mut body = first_clean[head.min(first_clean.len())..].to_string();
    for line in lines {
        body.push('\n');
        body.push_str(&dedent(line, content_at));
    }
    TaskItem {
        mark,
        checked,
        body: body.trim_end_matches(['\r', '\n']).to_string(),
    }
}

/// Remove up to `cols` columns of leading indent, tabs counting to the next
/// multiple of 4 (CommonMark's tab stop). A tab that straddles the cut leaves
/// its remainder as spaces, so nested lists keep their shape.
fn dedent(line: &str, cols: usize) -> String {
    let mut col = 0;
    for (i, ch) in line.char_indices() {
        if col >= cols {
            return line[i..].to_string();
        }
        match ch {
            ' ' => col += 1,
            '\t' => {
                let next = (col / 4 + 1) * 4;
                if next > cols {
                    return format!("{}{}", " ".repeat(next - cols), &line[i + 1..]);
                }
                col = next;
            }
            _ => return line[i..].to_string(),
        }
    }
    String::new()
}

/// Width of a bullet plus the whitespace after it (`- `, `12. `).
fn bullet_width(line: &str) -> usize {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let marker = if digits > 0 { digits + 1 } else { 1 };
    let after = line.get(marker..).unwrap_or("");
    marker + (after.len() - after.trim_start_matches([' ', '\t']).len())
}

/// Why a click didn't write.
#[derive(Debug, PartialEq)]
pub enum FlipError {
    /// The file on disk is not what the pane showed.
    Changed,
    /// The line moved or no longer reads as the task that was clicked.
    LineMoved,
}

/// Flip the box on file line `mark.line` of `fresh`, which must equal `shown`
/// (what the pane rendered). Returns the new file contents. Everything else,
/// line endings included, is byte-for-byte untouched.
pub fn flip(fresh: &str, shown: &str, mark: &TaskMark) -> Result<String, FlipError> {
    if fresh != shown {
        return Err(FlipError::Changed);
    }
    let start: usize = fresh
        .split_inclusive('\n')
        .take(mark.line)
        .map(str::len)
        .sum();
    let line = fresh[start..].split('\n').next().unwrap_or("");
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.trim() != mark.expected.trim() {
        return Err(FlipError::LineMoved);
    }
    match task_box(line) {
        Some((at, checked)) if checked == mark.checked => {
            let mut out = String::with_capacity(fresh.len());
            out.push_str(&fresh[..start + at]);
            out.push(if checked { ' ' } else { 'x' });
            out.push_str(&fresh[start + at + 1..]);
            Ok(out)
        }
        _ => Err(FlipError::LineMoved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_box_finds_the_box() {
        assert_eq!(task_box("- [ ] buy milk"), Some((3, false)));
        assert_eq!(task_box("  * [x] done"), Some((5, true)));
        assert_eq!(task_box("+ [X] done"), Some((3, true)));
        assert_eq!(task_box("12. [ ] numbered"), Some((5, false)));
        assert_eq!(task_box("1) [x] paren"), Some((4, true)));
        assert_eq!(task_box("- [x] [PM] bracketed text"), Some((3, true)));
    }

    #[test]
    fn task_box_rejects_non_tasks() {
        assert_eq!(task_box("- [ ]"), None);
        assert_eq!(task_box("- [ ]   "), None);
        assert_eq!(task_box("-[ ] no gap"), None);
        assert_eq!(task_box("- [y] other"), None);
        assert_eq!(task_box("- [ ]no space"), None);
        assert_eq!(task_box("- plain item"), None);
        assert_eq!(task_box("[ ] no bullet"), None);
        assert_eq!(task_box("1234567890. [ ] too many digits"), None);
        assert_eq!(task_box("- [é] multibyte"), None);
    }

    #[test]
    fn annotate_stamps_file_lines_and_skips_code() {
        let body = "# Todo\n- [ ] one\ntext\n```\n- [ ] sample\n```\n- [x] two\n  - [ ] kid\n";
        let out = annotate(body, 4);
        assert_eq!(
            out,
            "# Todo\n- [ ] <!--seance-task:5-->one\ntext\n```\n- [ ] sample\n```\n- [x] <!--seance-task:10-->two\n  - [ ] <!--seance-task:11-->kid\n"
        );
        assert_eq!(strip_markers(&out), body);
    }

    #[test]
    fn annotate_leaves_comments_code_and_quotes_alone() {
        for body in [
            "<!--\n- [ ] old\n- [x] also\n-->\n",
            "para\n\n    - [ ] indented code\n",
            "- item\n\n      ```\n      - [ ] nested fence\n      ```\n",
            "> - [ ] quoted\n",
            "\u{feff}- [ ] bom\n",
        ] {
            assert_eq!(annotate(body, 0), body, "{body:?}");
        }
    }

    #[test]
    fn annotate_handles_tabs_and_inline_backticks() {
        let body = "```x``` inline\n- [ ] a\n\t- [x] b\n";
        assert_eq!(
            annotate(body, 0),
            "```x``` inline\n- [ ] <!--seance-task:1-->a\n\t- [x] <!--seance-task:2-->b\n"
        );
    }

    #[test]
    fn annotate_keeps_crlf() {
        let out = annotate("- [ ] a\r\n- [x] b\r\n", 0);
        assert_eq!(
            out,
            "- [ ] <!--seance-task:0-->a\r\n- [x] <!--seance-task:1-->b\r\n"
        );
    }

    #[test]
    fn split_item_reads_box_and_dedents_children() {
        let src = "- [ ] <!--seance-task:7-->parent **bold**\n    more text\n    - [x] <!--seance-task:9-->child";
        let item = split_item(src, 2, Some(false));
        assert_eq!(
            item.mark,
            Some(TaskMark {
                line: 7,
                checked: false,
                expected: "- [ ] parent **bold**".into()
            })
        );
        assert_eq!(
            item.body,
            "parent **bold**\nmore text\n- [x] <!--seance-task:9-->child"
        );
    }

    #[test]
    fn split_item_plain_bullet() {
        let item = split_item("12. plain\n    next", 0, None);
        assert_eq!(item.mark, None);
        assert_eq!(item.body, "plain\nnext");
    }

    #[test]
    fn split_item_tab_children_and_unmarked_box() {
        let item = split_item(
            "- [ ] <!--seance-task:0-->parent\n\t- [ ] <!--seance-task:1-->kid\n\t- plain",
            0,
            Some(false),
        );
        assert_eq!(
            item.body,
            "parent\n  - [ ] <!--seance-task:1-->kid\n  - plain"
        );
        let item = split_item("- [x] no marker", 0, Some(true));
        assert_eq!(
            (item.mark, item.checked, item.body.as_str()),
            (None, Some(true), "no marker")
        );
    }

    #[test]
    fn split_item_indented_items_lose_their_lead() {
        let item = split_item("  - plain\n    more", 0, None);
        assert_eq!(item.body, "plain\nmore");
        let item = split_item("  - [x] done", 0, Some(true));
        assert_eq!((item.checked, item.body.as_str()), (Some(true), "done"));
        // Three tab levels: each nested body keeps a real nested list.
        let src = annotate("- [ ] a\n\t- [ ] b\n\t\t- [ ] c\n", 0);
        let a = split_item(src.trim_end(), 0, Some(false));
        assert_eq!(
            a.body,
            "a\n  - [ ] <!--seance-task:1-->b\n  \t- [ ] <!--seance-task:2-->c"
        );
        let b_src = &a.body["a\n".len()..];
        let b = split_item(b_src, 0, Some(false));
        assert_eq!(b.mark.map(|m| m.line), Some(1));
        assert_eq!(b.body, "b\n- [ ] <!--seance-task:2-->c");
    }

    #[test]
    fn flip_ignores_trailing_whitespace() {
        let doc = "  - [ ] b  \n";
        let b = TaskMark {
            line: 0,
            checked: false,
            expected: "- [ ] b".into(),
        };
        assert_eq!(flip(doc, doc, &b).unwrap(), "  - [x] b  \n");
    }

    #[test]
    fn flip_changes_one_byte() {
        let doc = "# T\r\n- [ ] a\r\n  - [x] b\r\n";
        let a = TaskMark {
            line: 1,
            checked: false,
            expected: "- [ ] a".into(),
        };
        assert_eq!(
            flip(doc, doc, &a).unwrap(),
            "# T\r\n- [x] a\r\n  - [x] b\r\n"
        );
        let b = TaskMark {
            line: 2,
            checked: true,
            expected: "- [x] b".into(),
        };
        assert_eq!(
            flip(doc, doc, &b).unwrap(),
            "# T\r\n- [ ] a\r\n  - [ ] b\r\n"
        );
    }

    #[test]
    fn flip_refuses_when_anything_moved() {
        let doc = "- [ ] a\n- [ ] b\n";
        let a = TaskMark {
            line: 0,
            checked: false,
            expected: "- [ ] a".into(),
        };
        assert_eq!(flip(doc, "- [ ] a\n", &a), Err(FlipError::Changed));
        let stale = TaskMark {
            line: 1,
            checked: false,
            expected: "- [ ] a".into(),
        };
        assert_eq!(flip(doc, doc, &stale), Err(FlipError::LineMoved));
        let wrong_state = TaskMark {
            line: 0,
            checked: true,
            expected: "- [ ] a".into(),
        };
        assert_eq!(flip(doc, doc, &wrong_state), Err(FlipError::LineMoved));
        let past_end = TaskMark {
            line: 9,
            checked: false,
            expected: "- [ ] a".into(),
        };
        assert_eq!(flip(doc, doc, &past_end), Err(FlipError::LineMoved));
    }
}
