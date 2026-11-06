//! Orchestrator verbs composed client-side from existing ops: `send` with
//! delivery confirmation, `note-agent`, `new --wait-ready --json
//! [--task-file]`, and `handoff`.
//!
//! Each is built from requests the daemon already serves (new / send /
//! send-raw / read / status / brief / task / kill), so they work against a
//! running daemon without an upgrade. Everything a script needs comes back
//! in one `--json` line; see docs/ORCHESTRATOR-ERGONOMICS.md.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use seance_core::agent_state::Activity;

use crate::control::{socket_path, ControlRequest, ControlResponse};

use super::deliver::{confirm, paste_raw, probe, take_deliver_flags, DeliverOpts, Probe};
use super::parse::{parse_new, parse_send, with_identity};
use super::print::print_ok_human;
use super::{send_request, ConnectError};

/// Exit code when the daemon accepted a send but delivery was not confirmed.
pub(super) const EXIT_UNDELIVERED: i32 = 3;

/// Identity every request in this module is stamped with.
#[derive(Clone)]
pub(super) struct Ctx {
    pub scope: Option<String>,
    pub from: Option<String>,
    pub json: bool,
}

impl Ctx {
    /// Round-trip one request. `Err` is the exit code, already reported.
    fn call(&self, req: ControlRequest) -> Result<Value, i32> {
        let req = with_identity(req, self.scope.clone(), self.from.clone());
        match send_request(&req) {
            Ok(ControlResponse { ok: true, data, .. }) => Ok(data.unwrap_or(Value::Null)),
            Ok(r) => Err(self.fail(1, r.error.as_deref().unwrap_or("request failed"), None)),
            Err(ConnectError::Connect(e)) => Err(self.fail(
                2,
                &format!(
                    "could not connect to control socket at {} ({e}); is seance running?",
                    socket_path().display()
                ),
                None,
            )),
            Err(ConnectError::Io(e)) => Err(self.fail(2, &format!("control IO error: {e}"), None)),
            Err(ConnectError::Protocol(e)) => {
                Err(self.fail(1, &format!("bad response: {e}"), None))
            }
        }
    }

    pub(super) fn call_public(&self, req: ControlRequest) -> Result<Value, i32> {
        self.call(req)
    }

    pub(super) fn fail_public(&self, code: i32, msg: &str) -> i32 {
        self.fail(code, msg, None)
    }

    /// Success: `data` as one JSON line under `--json`, else `human()`.
    pub(super) fn done_public(&self, human: impl FnOnce(), data: Value) -> i32 {
        if self.json {
            println!("{}", json!({"ok": true, "data": data}));
        } else {
            human();
        }
        0
    }

    /// Round-trip without reporting errors (best-effort follow-ups).
    fn call_quiet(&self, req: ControlRequest) -> Option<Value> {
        let req = with_identity(req, self.scope.clone(), self.from.clone());
        match send_request(&req) {
            Ok(ControlResponse { ok: true, data, .. }) => Some(data.unwrap_or(Value::Null)),
            _ => None,
        }
    }

    /// Report a failure (one JSON line under `--json`) and hand back `code`.
    fn fail(&self, code: i32, msg: &str, data: Option<&Value>) -> i32 {
        if self.json {
            let mut out = json!({"ok": false, "error": msg});
            if let Some(d) = data {
                out["data"] = d.clone();
            }
            println!("{out}");
        } else {
            eprintln!("seance ctl: {msg}");
        }
        code
    }

    fn done(&self, sub: &str, data: Value) -> i32 {
        if self.json {
            println!("{}", json!({"ok": true, "data": data}));
        } else {
            print_ok_human(
                sub,
                &ControlResponse {
                    ok: true,
                    data: Some(data),
                    error: None,
                },
            );
        }
        0
    }
}

fn help_or(msg: String) -> i32 {
    if let Some(help) = msg.strip_prefix("__help__\n") {
        println!("{help}");
        return 0;
    }
    eprintln!("seance ctl: {msg}");
    1
}

/// Refuse to paste into a modal: the text would answer the dialog.
fn modal_guard(ctx: &Ctx, pane: &str, before: &Probe) -> Result<(), i32> {
    match before.activity {
        Activity::AwaitingInput => Err(ctx.fail(
            1,
            &format!(
                "'{pane}' shows a prompt waiting for an answer ({}); answer it with \
                 `send-raw`, or pass --no-confirm to paste anyway",
                before.evidence.as_deref().unwrap_or("modal")
            ),
            None,
        )),
        Activity::Limited if !ctx.json => {
            eprintln!(
                "seance ctl: warning: '{pane}' reads usage-limited ({}); consider `ctl handoff`",
                before.evidence.as_deref().unwrap_or("?")
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `send PANE …` — the daemon opens the task and pastes; ctl confirms the
/// paste landed (see `deliver.rs`), re-pasting up to `--retry` times.
pub(super) fn run_send(mut args: Vec<String>, ctx: &Ctx) -> i32 {
    let opts = match take_deliver_flags(&mut args) {
        Ok(o) => o,
        Err(e) => return help_or(format!("send: {e}")),
    };
    let req = match parse_send(args) {
        Ok(r) => r,
        Err(e) => return help_or(e),
    };
    let ControlRequest::Send {
        pane, text, submit, ..
    } = &req
    else {
        unreachable!("parse_send returns Send");
    };
    let (pane, text, submit) = (pane.clone(), text.clone(), *submit);
    match send_task(ctx, req, &pane, &text, submit, &opts) {
        Ok(data) => ctx.done("send", data),
        Err(code) => code,
    }
}

/// Send + confirm; the response data gains `delivery`. Shared by `send`,
/// `new --task-file` and `handoff`.
///
/// Order matters (2026-09-28, v2-simplify-spec): look at the pane FIRST. A
/// busy pane (or any `--queue` send) is handed to the daemon's queue, which
/// delivers it when the pane goes idle. A send whose delivery can't be
/// confirmed is rolled back with `task_fail`, which reopens the task it had
/// parked or cancelled.
fn send_task(
    ctx: &Ctx,
    req: ControlRequest,
    pane: &str,
    text: &str,
    submit: bool,
    opts: &DeliverOpts,
) -> Result<Value, i32> {
    let before = if opts.confirm && submit {
        probe(pane, &ctx.scope, &ctx.from)
    } else {
        None
    };
    if let Some(b) = &before {
        modal_guard(ctx, pane, b)?;
    }
    let busy = before
        .as_ref()
        .is_some_and(|b| b.activity == Activity::Busy);
    // A busy pane: queued behind its turn, delivered when it goes idle.
    if opts.queue || busy {
        let row = ctx.call(ControlRequest::Status {
            pane: pane.to_string(),
            scope: None,
            from: None,
        })?;
        let Some(waiting) = row.get("queued_tasks").and_then(|q| q.as_array()) else {
            return Err(ctx.fail(
                1,
                "send: the pane is busy (or --queue) and this daemon predates the send \
                 queue; it needs `seance upgrade`",
                None,
            ));
        };
        // Idle with nothing ahead of it: deliver now. Otherwise keep order.
        if busy || !waiting.is_empty() || before.is_none() {
            let ControlRequest::Send {
                pane,
                text,
                submit,
                force,
                now,
                ..
            } = req
            else {
                unreachable!("send_task takes a Send");
            };
            return ctx.call(ControlRequest::Send {
                pane,
                text,
                submit,
                force,
                queue: true,
                now,
                scope: None,
                from: None,
            });
        }
    }
    let mut data = ctx.call(req)?;
    // The daemon queued it (the pane was asleep and is starting up): the pump
    // delivers and confirms it, so there is nothing here to confirm.
    if data.get("status").and_then(|s| s.as_str()) == Some("queued") {
        return Ok(data);
    }
    let Some(before) = before else {
        return Ok(data);
    };
    match confirm(pane, text, &before, opts, &ctx.scope, &ctx.from) {
        Ok(d) => {
            data["delivery"] = json!({"confirmed": true, "via": d.via, "attempts": d.attempts});
            Ok(data)
        }
        Err(e) => {
            data["delivery"] = json!({"confirmed": false});
            let tid = data["task_id"].as_str().unwrap_or("?").to_string();
            // Roll back: the task never landed, so it must not stay open
            // (and the one it cancelled comes back). Old daemons lack the op.
            let rollback = ctx.call_quiet(ControlRequest::TaskFail {
                id: tid.clone(),
                reason: Some("delivery not confirmed".into()),
                scope: None,
                from: None,
            });
            let tail = match &rollback {
                Some(r) => {
                    data["rolled_back"] = json!(true);
                    data["restored"] = r["restored"].clone();
                    format!(
                        "Task {tid} marked failed{}",
                        r["restored"]
                            .as_str()
                            .map(|t| format!("; {t} reopened"))
                            .unwrap_or_default()
                    )
                }
                None => format!(
                    "Task {tid} is open (daemon can't roll back); re-send or check the pane"
                ),
            };
            Err(ctx.fail(EXIT_UNDELIVERED, &format!("send: {e}. {tail}"), Some(&data)))
        }
    }
}

/// `note-agent PANE TEXT… | --file PATH | --stdin` — paste into the agent's
/// **current** task. No new task, no cancel, no status change: the text is
/// raw-pasted and submitted, then confirmed like `send`.
pub(super) fn run_note_agent(mut args: Vec<String>, ctx: &Ctx) -> i32 {
    let opts = match take_deliver_flags(&mut args) {
        Ok(o) => o,
        Err(e) => return help_or(format!("note-agent: {e}")),
    };
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "note-agent PANE TEXT... | --file PATH | --stdin\n  \
             paste a note into the pane's running task (no new task, no cancel)\n  \
             --interrupt  if Claude queues it behind a running turn, deliver it NOW by\n  \
                          interrupting the turn (cancels the running tool call; the agent\n  \
                          answers and stops — re-send its task if it should continue)\n  \
             --no-submit  --no-confirm  --retry N  --confirm-secs S"
        );
        return 0;
    }
    let req = match parse_send(args) {
        Ok(r) => r,
        Err(e) => return help_or(e.replacen("send", "note-agent", 1)),
    };
    let ControlRequest::Send {
        pane, text, submit, ..
    } = req
    else {
        unreachable!("parse_send returns Send");
    };
    let before = if opts.confirm && submit {
        probe(&pane, &ctx.scope, &ctx.from)
    } else {
        None
    };
    if let Some(b) = &before {
        if let Err(code) = modal_guard(ctx, &pane, b) {
            return code;
        }
    }
    // Resolve the slug + open task for the reply (and to fail on a bad pane
    // before writing anything).
    let row = match ctx.call(ControlRequest::Status {
        pane: pane.clone(),
        scope: None,
        from: None,
    }) {
        Ok(r) => r,
        Err(code) => return code,
    };
    if let Err(e) = paste_raw(&pane, &text, submit, &ctx.scope, &ctx.from) {
        return ctx.fail(1, &format!("note-agent: {e}"), None);
    }
    let mut data = json!({
        "slug": row.get("slug").cloned().unwrap_or(json!(pane)),
        "task_id": row.get("task_id").cloned().unwrap_or(Value::Null),
        "status": "noted",
    });
    if let Some(before) = before {
        match confirm(&pane, &text, &before, &opts, &ctx.scope, &ctx.from) {
            Ok(d) => {
                let mut via = d.via;
                // Claude parked it behind the running turn (e.g. the main
                // agent is waiting on a subagent). --interrupt presses Claude's
                // "ctrl+x ctrl+s to send now", which cancels the in-flight
                // tool call and ends the turn (verified live 2026-09-28).
                let still_queued = via == "queued"
                    || probe(&pane, &ctx.scope, &ctx.from).is_some_and(|p| p.queued_input);
                if still_queued && opts.interrupt {
                    via = send_queued_now(&pane, ctx);
                }
                data["delivery"] = json!({"confirmed": true, "via": via, "attempts": d.attempts});
                if (via == "queued" || (still_queued && via != "interrupted")) && !ctx.json {
                    eprintln!(
                        "note: queued behind '{pane}''s running turn — the agent reads it when \
                         the turn yields (status shows queued_input=true). --interrupt delivers \
                         it now by interrupting the turn (cancels the running tool call)."
                    );
                }
            }
            Err(e) => {
                data["delivery"] = json!({"confirmed": false});
                return ctx.fail(EXIT_UNDELIVERED, &format!("note-agent: {e}"), Some(&data));
            }
        }
    }
    ctx.done("send", data)
}

/// Press ctrl+x ctrl+s (Claude: "send now") and wait for the queued-input
/// marker to clear. Claude interrupts the running turn to do it — the tool
/// call in flight is cancelled and the agent does not resume on its own.
/// Returns the delivery label.
fn send_queued_now(pane: &str, ctx: &Ctx) -> &'static str {
    if super::deliver::raw(pane, b"\x18\x13", &ctx.scope, &ctx.from).is_err() {
        return "queued";
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(300));
        if probe(pane, &ctx.scope, &ctx.from).is_some_and(|p| !p.queued_input) {
            return "interrupted";
        }
    }
    "queued"
}

/// Block until a freshly spawned pane can take a paste: its process runs,
/// boot dialogs are dealt with, and the screen reads `idle` (agents) or
/// holds still (shells) for `stable`.
///
/// Update menus are skipped. A **trust** dialog is accepted only with
/// `trust` (`--trust`); otherwise this fails at once naming it — before
/// 9f5f444 the wait sat ~2 min and the first send's Enter picked Claude's
/// default "No, exit". Any other modal is left alone and reported.
pub(super) fn wait_boot_ready(
    ctx: &Ctx,
    slug: &str,
    command: &str,
    timeout: Duration,
    trust: bool,
) -> Result<(), String> {
    use crate::agents::{boot_dialog, boot_dialog_step, BootDialog};
    let stable = Duration::from_millis(1200);
    let started = Instant::now();
    let profile = crate::agents::guess_profile_from_command(command).unwrap_or("");
    let is_agent = !profile.is_empty() && profile != "shell";
    let mut steps = 0;
    let mut last = String::new();
    let mut since = Instant::now();
    while started.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(300));
        let Some(p) = probe(slug, &ctx.scope, &ctx.from) else {
            continue;
        };
        if let Some(dialog) = boot_dialog(&p.screen) {
            if dialog == BootDialog::Trust && !trust {
                return Err(format!(
                    "'{slug}' is asking whether to trust its folder ({}). Nothing was sent; \
                     the pane is left at that prompt. Re-run with --trust to accept it, or \
                     answer it in the pane",
                    p.evidence.as_deref().unwrap_or("trust dialog")
                ));
            }
            // One verified step at a time: the next probe decides the next key.
            let Some(keys) = boot_dialog_step(&p.screen, dialog).filter(|_| steps < 8) else {
                return Err(format!(
                    "'{slug}' shows a boot dialog ctl can't navigate ({}); answer it in the pane",
                    p.evidence.as_deref().unwrap_or("?")
                ));
            };
            steps += 1;
            if !ctx.json {
                eprintln!(
                    "boot-clear '{slug}': {dialog:?} → {}",
                    String::from_utf8_lossy(keys).escape_debug()
                );
            }
            super::deliver::raw(slug, keys, &ctx.scope, &ctx.from)
                .map_err(|e| format!("'{slug}': could not answer its boot dialog: {e}"))?;
            std::thread::sleep(Duration::from_millis(400));
            continue;
        }
        if p.screen != last {
            last = p.screen.clone();
            since = Instant::now();
            continue;
        }
        let settled = since.elapsed() >= stable;
        let ok = if is_agent {
            p.activity == Activity::Idle
        } else {
            !p.screen.trim().is_empty()
        };
        if settled && ok {
            return Ok(());
        }
    }
    Err(format!(
        "'{slug}' not ready after {}s (last screen tail: {:?})",
        timeout.as_secs(),
        last.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
    ))
}

/// Pull `name VALUE`-style flags for `new` that the daemon never sees.
struct NewExtras {
    trust: bool,
    task_file: Option<String>,
    task_stdin: bool,
    ready_secs: u64,
}

fn take_new_extras(args: &mut Vec<String>) -> Result<NewExtras, String> {
    let mut x = NewExtras {
        trust: false,
        task_file: None,
        task_stdin: false,
        ready_secs: 180,
    };
    let mut rest = Vec::with_capacity(args.len());
    let mut it = std::mem::take(args).into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--task-file" => x.task_file = Some(it.next().ok_or("new: --task-file needs PATH")?),
            "--task-stdin" => x.task_stdin = true,
            "--trust" => x.trust = true,
            "--ready-timeout" => {
                x.ready_secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("new: --ready-timeout needs seconds")?
            }
            _ => rest.push(a),
        }
    }
    *args = rest;
    Ok(x)
}

/// `new … [--wait-ready] [--task-file PATH|--task-stdin] [--json]` — spawn,
/// optionally wait for boot, optionally send a first task (confirmed). One
/// result: `{slug, name, requested_name, workspace, cwd, command, parent,
/// scratchpad, ready?, task_id?, delivery?}`. A task implies `--wait-ready`.
pub(super) fn run_new(mut args: Vec<String>, wait_ready: bool, ctx: &Ctx) -> i32 {
    let opts = match take_deliver_flags(&mut args) {
        Ok(o) => o,
        Err(e) => return help_or(format!("new: {e}")),
    };
    let extras = match take_new_extras(&mut args) {
        Ok(x) => x,
        Err(e) => return help_or(e),
    };
    let task_text = match (&extras.task_file, extras.task_stdin) {
        (Some(path), _) => match std::fs::read_to_string(path) {
            Ok(t) => Some(t),
            Err(e) => return ctx.fail(1, &format!("new: read {path}: {e}"), None),
        },
        (None, true) => {
            use std::io::Read;
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                return ctx.fail(1, &format!("new: stdin: {e}"), None);
            }
            Some(buf)
        }
        (None, false) => None,
    };
    let req = match parse_new(args) {
        Ok(r) => r,
        Err(e) => return help_or(e),
    };
    let ControlRequest::New {
        name: requested,
        command: req_command,
        ..
    } = &req
    else {
        unreachable!("parse_new returns New");
    };
    let (requested, req_command) = (requested.clone(), req_command.clone().unwrap_or_default());
    let mut data = match ctx.call(req) {
        Ok(d) => d,
        Err(code) => return code,
    };
    let slug = data["slug"].as_str().unwrap_or_default().to_string();
    // Older daemons answer {slug, workspace, scratchpad} only.
    if data.get("name").is_none() {
        data["name"] = json!(requested);
    }
    if data.get("command").is_none() {
        data["command"] = json!(req_command);
    }
    data["requested_name"] = json!(requested);
    let command = data["command"].as_str().unwrap_or_default().to_string();
    if !ctx.json {
        print_ok_human("new", &ControlResponse::ok(data.clone()));
        if slug != requested {
            eprintln!("note: '{requested}' was taken; this pane's id is '{slug}'");
        }
    }
    if wait_ready || task_text.is_some() {
        if !ctx.json {
            eprintln!("waiting until '{slug}' is boot-ready…");
        }
        if let Err(e) = wait_boot_ready(
            ctx,
            &slug,
            &command,
            Duration::from_secs(extras.ready_secs),
            extras.trust,
        ) {
            data["ready"] = json!(false);
            return ctx.fail(1, &format!("new: {e}"), Some(&data));
        }
        data["ready"] = json!(true);
        if !ctx.json {
            println!("ready {slug}");
        }
    }
    if let Some(text) = task_text {
        let req = ControlRequest::Send {
            pane: slug.clone(),
            text: text.clone(),
            submit: true,
            force: false,
            queue: false,
            now: false,
            scope: None,
            from: None,
        };
        match send_task(ctx, req, &slug, &text, true, &opts) {
            Ok(sent) => {
                data["task_id"] = sent["task_id"].clone();
                data["delivery"] = sent["delivery"].clone();
                if !ctx.json {
                    print_ok_human("send", &ControlResponse::ok(sent));
                }
            }
            Err(code) => return code,
        }
    }
    if ctx.json {
        println!("{}", json!({"ok": true, "data": data}));
    }
    0
}

/// Takeover preamble for `handoff`.
pub(super) fn takeover_text(
    old: &str,
    new: &str,
    task_id: &str,
    cwd: &str,
    note: Option<&str>,
    body: &str,
) -> String {
    let mut t = format!(
        "TAKEOVER: you are taking over seance task {task_id} from pane '{old}', which stopped \
         partway. You are pane '{new}'; wherever the task below names '{old}', that now means \
         you. Before anything else, inspect the working tree(s) it touches (start in {cwd}) with \
         git status, git diff and git log: keep committed work, finish or redo uncommitted work \
         (never discard it blindly), then complete the task exactly.\n"
    );
    if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
        t.push_str(&format!("\nNOTE FROM THE COORDINATOR: {}\n", n.trim()));
    }
    t.push_str("\n---- original task ----\n");
    t.push_str(body);
    t
}

/// `handoff PANE [--agent NAME] [--name NEW] [--note TEXT] [--keep]` — move
/// a pane's open task to a fresh agent in the same cwd + circle: spawn the
/// successor and wait for boot, kill the original (one writer per worktree;
/// `--keep` skips), re-send the task with a takeover preamble, confirm it.
pub(super) fn run_handoff(args: Vec<String>, ctx: &Ctx) -> i32 {
    let mut pane = None;
    let mut agent = "claude".to_string();
    let mut name = None;
    let mut note = None;
    let mut keep = false;
    let mut trust = false;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agent" => agent = it.next().unwrap_or_default(),
            "--name" => name = it.next(),
            "--note" => note = it.next(),
            "--keep" => keep = true,
            "--trust" => trust = true,
            "--help" | "-h" => {
                println!(
                    "handoff PANE [--agent claude] [--name NEW] [--note TEXT] [--keep] [--trust]\n  \
                     spawn a same-cwd successor, re-send PANE's open task with a takeover\n  \
                     preamble, then kill PANE and hand its queued tasks over (unless --keep).\n  \
                     Prints the new slug + task id."
                );
                return 0;
            }
            other if pane.is_none() && !other.starts_with('-') => pane = Some(other.to_string()),
            other => return ctx.fail(1, &format!("handoff: unexpected '{other}'"), None),
        }
    }
    let Some(pane) = pane else {
        return ctx.fail(1, "handoff: expected PANE", None);
    };
    let profile = match crate::agents::resolve(&agent) {
        Ok(p) => p,
        Err(e) => return ctx.fail(1, &format!("handoff: {e}"), None),
    };
    let row = match ctx.call(ControlRequest::Status {
        pane: pane.clone(),
        scope: None,
        from: None,
    }) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let old = row["slug"].as_str().unwrap_or(&pane).to_string();
    // `cwd` joined `status` with this change; a daemon that predates it still
    // carries it on the brief row.
    let cwd = match row["cwd"].as_str() {
        Some(c) => c.to_string(),
        None => match brief_cwd(ctx, &old) {
            Ok(c) => c,
            Err(code) => return code,
        },
    };
    let workspace = row["workspace"].as_str().unwrap_or_default().to_string();
    let old_name = row["name"].as_str().unwrap_or(&old).to_string();
    let task = match ctx.call(ControlRequest::Task {
        pane: Some(pane.clone()),
        id: None,
        scope: None,
        from: None,
    }) {
        Ok(t) if t["status"] == "open" => t,
        Ok(t) => {
            return ctx.fail(
                1,
                &format!(
                    "handoff: '{old}' has no open task (latest {} is {}); nothing to hand off",
                    t["id"].as_str().unwrap_or("?"),
                    t["status"].as_str().unwrap_or("?")
                ),
                None,
            )
        }
        Err(code) => return code,
    };
    let task_id = task["id"].as_str().unwrap_or("?").to_string();
    let body = task["body"].as_str().unwrap_or_default().to_string();

    let new_req = ControlRequest::New {
        name: name.unwrap_or_else(|| format!("{old_name}-{}", profile.name)),
        cwd: Some(cwd.clone()),
        command: Some(crate::agents::command_line(&profile)),
        workspace: Some(workspace.clone()),
        file: None,
        scope: None,
        from: None,
    };
    let spawned = match ctx.call(new_req) {
        Ok(d) => d,
        Err(code) => return code,
    };
    let new = spawned["slug"].as_str().unwrap_or_default().to_string();
    let command = crate::agents::command_line(&profile);
    if let Err(e) = wait_boot_ready(ctx, &new, &command, Duration::from_secs(180), trust) {
        return ctx.fail(
            1,
            &format!("handoff: successor {e}; '{old}' left running"),
            Some(&json!({"slug": new, "old": old})),
        );
    }
    let text = takeover_text(&old, &new, &task_id, &cwd, note.as_deref(), &body);
    // `now`: if the successor reads busy and this gets queued, it still goes
    // ahead of the queue handed over below (it has no task to cancel).
    let req = ControlRequest::Send {
        pane: new.clone(),
        text: text.clone(),
        submit: true,
        force: false,
        queue: false,
        now: true,
        scope: None,
        from: None,
    };
    let sent = match send_task(ctx, req, &new, &text, true, &DeliverOpts::default()) {
        Ok(s) => s,
        Err(code) => return code,
    };
    // After the takeover is in, so the old pane's queued tasks (handed to
    // the successor here) line up behind it.
    if !keep {
        if let Err(code) = ctx.call(ControlRequest::Kill {
            pane: pane.clone(),
            queue_to: Some(new.clone()),
            scope: None,
            from: None,
        }) {
            return code;
        }
    }
    let data = json!({
        "old": old,
        "old_killed": !keep,
        "from_task": task_id,
        "slug": new,
        "pane_id": spawned["pane_id"].clone(),
        "workspace_id": spawned["workspace_id"].clone(),
        "name": spawned["name"].clone(),
        "workspace": workspace,
        "cwd": cwd,
        "agent": profile.name,
        "task_id": sent["task_id"].clone(),
        "delivery": sent["delivery"].clone(),
    });
    if ctx.json {
        println!("{}", json!({"ok": true, "data": data}));
    } else {
        println!(
            "handoff {old} → {new} task={} (from {task_id}){}",
            data["task_id"].as_str().unwrap_or("?"),
            if keep { "" } else { "; original killed" }
        );
    }
    0
}

fn brief_cwd(ctx: &Ctx, slug: &str) -> Result<String, i32> {
    let brief = ctx.call(ControlRequest::Brief {
        scope: None,
        from: None,
    })?;
    brief["panes"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["slug"] == slug))
        .and_then(|r| r["cwd"].as_str())
        .map(String::from)
        .ok_or_else(|| ctx.fail(1, &format!("handoff: no cwd for '{slug}'"), None))
}

/// `roster` narrowing by spawn lineage (panes carry `parent` since 0.27).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct RosterFilter {
    /// Only panes spawned by this pane (`--parent PANE`, `--children-of`).
    pub parent: Option<String>,
    /// Hide panes that were spawned by another pane (`--top`).
    pub top_only: bool,
}

pub(super) fn parse_roster_filter(args: &[String]) -> Result<RosterFilter, String> {
    let mut f = RosterFilter::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--parent" | "--children-of" => {
                f.parent = Some(it.next().ok_or("roster: --parent needs PANE")?.clone())
            }
            "--top" | "--no-children" => f.top_only = true,
            other => {
                return Err(format!(
                    "roster: unexpected '{other}' (--parent PANE | --top)"
                ))
            }
        }
    }
    Ok(f)
}

pub(super) fn roster_keep(f: &RosterFilter, row: &Value) -> bool {
    let parent = row["parent"].as_str();
    if f.top_only && parent.is_some() {
        return false;
    }
    match &f.parent {
        Some(want) => parent == Some(want.as_str()),
        None => true,
    }
}

pub(super) fn run_roster(mut filter: RosterFilter, ctx: &Ctx) -> i32 {
    if let Some(id) = filter.parent.as_ref().filter(|p| p.starts_with("id:")) {
        let parent = match ctx.call(ControlRequest::Status {
            pane: id.clone(),
            scope: None,
            from: None,
        }) {
            Ok(row) => row,
            Err(code) => return code,
        };
        filter.parent = parent["slug"].as_str().map(str::to_string);
    }
    let mut data = match ctx.call(ControlRequest::Roster {
        scope: None,
        from: None,
    }) {
        Ok(d) => d,
        Err(code) => return code,
    };
    if let Some(rows) = data["panes"].as_array_mut() {
        rows.retain(|r| roster_keep(&filter, r));
    }
    ctx.done("roster", data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_extras_strip_client_flags() {
        let mut args: Vec<String> = [
            "--name",
            "w",
            "--task-file",
            "/t.md",
            "--agent",
            "claude",
            "--ready-timeout",
            "60",
            "--trust",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let x = take_new_extras(&mut args).unwrap();
        assert_eq!(x.task_file.as_deref(), Some("/t.md"));
        assert_eq!(x.ready_secs, 60);
        assert!(!x.task_stdin);
        assert!(x.trust);
        assert_eq!(args, ["--name", "w", "--agent", "claude"]);
        assert!(parse_new(args).is_ok(), "leftovers parse as a plain new");
    }

    #[test]
    fn takeover_text_names_both_panes_and_keeps_body() {
        let t = takeover_text(
            "v2-ios-a",
            "v2-ios-a-claude",
            "task-7",
            "/w/ios",
            Some("codex hit its weekly limit"),
            "BUILD lane ios-a\nWHEN DONE: write result",
        );
        assert!(t.starts_with("TAKEOVER:"));
        assert!(
            t.contains("task-7") && t.contains("'v2-ios-a'") && t.contains("'v2-ios-a-claude'")
        );
        assert!(t.contains("start in /w/ios"));
        assert!(t.contains("NOTE FROM THE COORDINATOR: codex hit its weekly limit"));
        assert!(t.ends_with("BUILD lane ios-a\nWHEN DONE: write result"));
        assert!(!takeover_text("a", "b", "t", "/", Some("  "), "x").contains("NOTE"));
    }

    #[test]
    fn roster_filter_by_lineage() {
        let f = parse_roster_filter(&["--parent".into(), "orch".into()]).unwrap();
        assert!(roster_keep(&f, &json!({"slug": "w", "parent": "orch"})));
        assert!(!roster_keep(&f, &json!({"slug": "w", "parent": "other"})));
        assert!(!roster_keep(&f, &json!({"slug": "orch"})));
        let top = parse_roster_filter(&["--top".into()]).unwrap();
        assert!(roster_keep(&top, &json!({"slug": "orch", "parent": null})));
        assert!(!roster_keep(&top, &json!({"slug": "w", "parent": "orch"})));
        assert!(roster_keep(
            &RosterFilter::default(),
            &json!({"parent": "x"})
        ));
        assert!(parse_roster_filter(&["--bogus".into()]).is_err());
    }
}
