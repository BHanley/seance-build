//! Session-to-session messaging verbs (docs/COMMS.md): `ask --to`, `tell`,
//! `reply`, `await`, `messages`, `lead`, `contacts`.
//!
//! `ask --to X` is an awaited command: it prints the answer on stdout when
//! the other session replies (run it in the background to keep working —
//! Claude Code wakes you when it exits). If nobody is waiting when the reply
//! comes, the daemon pastes it into the asker's pane instead, so an answer is
//! never lost to a timeout. Plain `ask "Q"` (no `--to`) is still the question
//! to the human.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::control::ControlRequest;

use super::agentops::Ctx;

/// Exit code when `ask`/`await` gave up waiting (the answer can still come).
const EXIT_NO_ANSWER_YET: i32 = 5;

fn body(
    words: Vec<String>,
    file: Option<String>,
    stdin: bool,
    verb: &str,
) -> Result<String, String> {
    use std::io::IsTerminal;
    body_with(words, file, stdin, !std::io::stdin().is_terminal(), verb)
}

/// `stdin_piped`: stdin is not a terminal (a heredoc or `< file`). Injected
/// so tests never read the test runner's own stdin.
fn body_with(
    words: Vec<String>,
    file: Option<String>,
    stdin: bool,
    stdin_piped: bool,
    verb: &str,
) -> Result<String, String> {
    // No TEXT and piped input (a heredoc, `< file`): read it without making
    // the caller remember --stdin — the pasted instructions say
    // `reply m-N <<'EOF'`, and an agent followed them literally.
    let piped = words.is_empty() && file.is_none() && stdin_piped;
    let text = if let Some(path) = file {
        std::fs::read_to_string(&path).map_err(|e| format!("{verb}: read {path}: {e}"))?
    } else if stdin || piped {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("{verb}: stdin: {e}"))?;
        buf
    } else {
        words.join(" ")
    };
    if text.trim().is_empty() {
        return Err(format!(
            "{verb}: no text (give TEXT, --file PATH or --stdin)"
        ));
    }
    Ok(text)
}

struct Parsed {
    to: Option<String>,
    words: Vec<String>,
    file: Option<String>,
    stdin: bool,
    wait: bool,
    timeout: u64,
    limit: Option<usize>,
    circle: Option<String>,
    pane: Option<String>,
    set: Option<String>,
}

fn parse(args: Vec<String>, verb: &str) -> Result<Parsed, String> {
    let mut p = Parsed {
        to: None,
        words: Vec::new(),
        file: None,
        stdin: false,
        wait: true,
        timeout: 600,
        limit: None,
        circle: None,
        pane: None,
        set: None,
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{verb}: {name} needs a value"));
        match a.as_str() {
            "--to" => p.to = Some(val("--to")?),
            "--file" => p.file = Some(val("--file")?),
            "--stdin" => p.stdin = true,
            "--no-wait" => p.wait = false,
            "--timeout" => {
                p.timeout = val("--timeout")?
                    .parse()
                    .map_err(|_| format!("{verb}: --timeout needs seconds"))?
            }
            "--limit" => {
                p.limit = Some(
                    val("--limit")?
                        .parse()
                        .map_err(|_| format!("{verb}: --limit needs a number"))?,
                )
            }
            "--circle" => p.circle = Some(val("--circle")?),
            "--pane" => p.pane = Some(val("--pane")?),
            "--set" => p.set = Some(val("--set")?),
            _ => p.words.push(a),
        }
    }
    Ok(p)
}

fn fail(ctx: &Ctx, msg: &str) -> i32 {
    ctx.fail_public(1, msg)
}

/// `ask --to TARGET TEXT… | --file | --stdin [--no-wait] [--timeout S]`
pub(super) fn run_ask_to(args: Vec<String>, ctx: &Ctx) -> i32 {
    let p = match parse(args, "ask") {
        Ok(p) => p,
        Err(e) => return fail(ctx, &e),
    };
    let Some(to) = p.to.clone() else {
        return fail(ctx, "ask: --to CIRCLE|PANE is required");
    };
    let text = match body(p.words, p.file, p.stdin, "ask") {
        Ok(t) => t,
        Err(e) => return fail(ctx, &e),
    };
    let sent = match send(ctx, &to, &text, "ask") {
        Ok(v) => v,
        Err(code) => return code,
    };
    let id = sent["id"].as_str().unwrap_or_default().to_string();
    if !p.wait {
        return ctx.done_public(|| {
            println!(
                "asked {id} → {} ({}); the reply will be pasted into your pane, or: seance ctl await {id}",
                sent["to_circle_label"].as_str().unwrap_or("?"),
                sent["to_pane"].as_str().unwrap_or("?")
            )
        }, sent.clone());
    }
    if !ctx.json {
        eprintln!(
            "asked {id} → {} ({}); waiting for the reply…",
            sent["to_circle_label"].as_str().unwrap_or("?"),
            sent["to_pane"].as_str().unwrap_or("?")
        );
    }
    await_answer(ctx, &id, p.timeout)
}

fn send(ctx: &Ctx, to: &str, text: &str, kind: &str) -> Result<Value, i32> {
    let v = ctx.call_public(ControlRequest::MsgSend {
        to: to.to_string(),
        text: text.to_string(),
        kind: kind.to_string(),
        scope: None,
        from: None,
    })?;
    if let Some(note) = v["note"].as_str() {
        if !ctx.json {
            eprintln!("note: {note}");
        }
    }
    Ok(v)
}

/// Block until message `id` is answered (or failed / timed out).
fn await_answer(ctx: &Ctx, id: &str, timeout: u64) -> i32 {
    let started = Instant::now();
    loop {
        let m = match ctx.call_public(ControlRequest::MsgGet {
            id: id.to_string(),
            waiting: true,
            scope: None,
            from: None,
        }) {
            Ok(m) => m,
            Err(code) => return code,
        };
        match m["status"].as_str() {
            Some("answered") => {
                let answer = m["answer"].as_str().unwrap_or_default().to_string();
                return ctx.done_public(
                    || {
                        print!("{answer}");
                        if !answer.ends_with('\n') {
                            println!();
                        }
                    },
                    m.clone(),
                );
            }
            Some("failed") => {
                return fail(
                    ctx,
                    &format!(
                        "{id} could not be delivered: {}",
                        m["note"].as_str().unwrap_or("unknown reason")
                    ),
                )
            }
            _ => {}
        }
        if started.elapsed() >= Duration::from_secs(timeout) {
            return ctx.fail_public(
                EXIT_NO_ANSWER_YET,
                &format!(
                    "no answer to {id} after {timeout}s ({}). It will be pasted into your \
                     pane when it comes, or: seance ctl await {id}",
                    m["status"].as_str().unwrap_or("?")
                ),
            );
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}

/// `tell TARGET TEXT… | --file | --stdin`
pub(super) fn run_tell(args: Vec<String>, ctx: &Ctx) -> i32 {
    let mut p = match parse(args, "tell") {
        Ok(p) => p,
        Err(e) => return fail(ctx, &e),
    };
    let to = match p.to.take() {
        Some(t) => t,
        None if !p.words.is_empty() => p.words.remove(0),
        None => return fail(ctx, "tell: expected TARGET (a circle or pane) and TEXT"),
    };
    let text = match body(p.words, p.file, p.stdin, "tell") {
        Ok(t) => t,
        Err(e) => return fail(ctx, &e),
    };
    match send(ctx, &to, &text, "tell") {
        Ok(v) => ctx.done_public(
            || {
                println!(
                    "told {} → {} ({})",
                    v["id"].as_str().unwrap_or("?"),
                    v["to_circle_label"].as_str().unwrap_or("?"),
                    v["to_pane"].as_str().unwrap_or("?")
                )
            },
            v.clone(),
        ),
        Err(code) => code,
    }
}

/// `reply ID TEXT… | --file | --stdin`
pub(super) fn run_reply(args: Vec<String>, ctx: &Ctx) -> i32 {
    let mut p = match parse(args, "reply") {
        Ok(p) => p,
        Err(e) => return fail(ctx, &e),
    };
    if p.words.is_empty() {
        return fail(ctx, "reply: expected MESSAGE-ID (e.g. m-12) and TEXT");
    }
    let id = p.words.remove(0);
    let text = match body(p.words, p.file, p.stdin, "reply") {
        Ok(t) => t,
        Err(e) => return fail(ctx, &e),
    };
    match ctx.call_public(ControlRequest::MsgReply {
        id: id.clone(),
        text,
        scope: None,
        from: None,
    }) {
        Ok(v) => ctx.done_public(
            || {
                let how = if v["to_waiting_caller"] == true {
                    "the asker was waiting and has it".to_string()
                } else if let Some(p) = v["pasted"]["pane"].as_str() {
                    format!("delivering it to {p}")
                } else if v["asker"].is_null() {
                    format!("stored; it was asked from outside a pane (`seance ctl await {id}` shows it)")
                } else {
                    format!("stored; the asker's pane is gone (`seance ctl await {id}` shows it)")
                };
                println!("replied to {id}: {how}");
            },
            v.clone(),
        ),
        Err(code) => code,
    }
}

/// `await ID [--timeout S]`
pub(super) fn run_await(args: Vec<String>, ctx: &Ctx) -> i32 {
    let mut p = match parse(args, "await") {
        Ok(p) => p,
        Err(e) => return fail(ctx, &e),
    };
    if p.words.is_empty() {
        return fail(ctx, "await: expected MESSAGE-ID");
    }
    let id = p.words.remove(0);
    await_answer(ctx, &id, p.timeout)
}

/// `messages [--circle C | --pane P] [--limit N]`
pub(super) fn run_messages(args: Vec<String>, ctx: &Ctx) -> i32 {
    let p = match parse(args, "messages") {
        Ok(p) => p,
        Err(e) => return fail(ctx, &e),
    };
    let v = match ctx.call_public(ControlRequest::MsgList {
        pane: p.pane,
        circle: p.circle,
        limit: p.limit,
        scope: None,
        from: None,
    }) {
        Ok(v) => v,
        Err(code) => return code,
    };
    ctx.done_public(
        || {
            let rows = v["messages"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("(no messages)");
            }
            for m in rows {
                let from = m["from_pane"].as_str().unwrap_or("-");
                let to = m["to_pane"].as_str().unwrap_or("-");
                let first = m["text"]
                    .as_str()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("");
                println!(
                    "{:<6} {:<6} {:<9} {} → {}  {}",
                    m["id"].as_str().unwrap_or("?"),
                    m["kind"].as_str().unwrap_or("?"),
                    m["status"].as_str().unwrap_or("?"),
                    format_args!(
                        "{} ({from})",
                        m["from_circle_label"].as_str().unwrap_or("-")
                    ),
                    format_args!("{} ({to})", m["to_circle_label"].as_str().unwrap_or("-")),
                    first.chars().take(60).collect::<String>()
                );
                if let Some(a) = m["answer"].as_str() {
                    println!(
                        "       ↳ {}",
                        a.lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(70)
                            .collect::<String>()
                    );
                }
            }
        },
        v.clone(),
    )
}

/// `lead [CIRCLE] [--set PANE]`
pub(super) fn run_lead(args: Vec<String>, ctx: &Ctx) -> i32 {
    let mut p = match parse(args, "lead") {
        Ok(p) => p,
        Err(e) => return fail(ctx, &e),
    };
    let circle = if p.words.is_empty() {
        None
    } else {
        Some(p.words.remove(0))
    };
    match ctx.call_public(ControlRequest::Lead {
        workspace: circle,
        pane: p.set,
        scope: None,
        from: None,
    }) {
        Ok(v) => ctx.done_public(
            || {
                println!(
                    "{} (slug {}): lead {}{}",
                    v["workspace_name"].as_str().unwrap_or("?"),
                    v["workspace"].as_str().unwrap_or("?"),
                    v["lead"].as_str().unwrap_or("-"),
                    if v["explicit"] == true {
                        " (set)"
                    } else {
                        " (first pane)"
                    }
                )
            },
            v.clone(),
        ),
        Err(code) => code,
    }
}

/// `contacts [PANE]`
pub(super) fn run_contacts(args: Vec<String>, ctx: &Ctx) -> i32 {
    let v = match ctx.call_public(ControlRequest::Contacts {
        pane: args.into_iter().next(),
        scope: None,
        from: None,
    }) {
        Ok(v) => v,
        Err(code) => return code,
    };
    ctx.done_public(
        || {
            let rows = v["contacts"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("(no contacts)");
            }
            for c in rows {
                println!(
                    "{} (slug {}) lead {}",
                    c["workspace_name"].as_str().unwrap_or("?"),
                    c["workspace"].as_str().unwrap_or("?"),
                    c["lead"].as_str().unwrap_or("-")
                );
            }
        },
        json!(v),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ask_and_reply_shapes() {
        let p = parse(
            [
                "--to",
                "paceline-arch-impl",
                "which",
                "option?",
                "--no-wait",
                "--timeout",
                "30",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            "ask",
        )
        .unwrap();
        assert_eq!(p.to.as_deref(), Some("paceline-arch-impl"));
        assert_eq!(p.words, ["which", "option?"]);
        assert!(!p.wait);
        assert_eq!(p.timeout, 30);
        assert!(parse(vec!["--timeout".into(), "x".into()], "ask").is_err());
        assert_eq!(
            body_with(vec!["a".into(), "b".into()], None, false, true, "reply").unwrap(),
            "a b"
        );
        assert!(body_with(vec![], None, false, false, "reply").is_err());
    }
}
