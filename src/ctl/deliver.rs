//! Delivery confirmation for paste-style injects (`send`, `note-agent`).
//!
//! The daemon's `send` writes a bracketed paste and a CR into the PTY and
//! answers `ok` — it cannot know whether the TUI took it. On 2026-09-27 two
//! panes sent tasks right after `--wait-ready` never got them (empty prompt,
//! task open, no error), and an idle Codex pane missed one too. So ctl now
//! watches the pane after the write, using the same classifier the daemon
//! reports (`seance_core::agent_state`), and says which of these happened:
//!
//! - **busy** — the agent left idle (spinner / "Working (…)"),
//! - **modal** — it reacted by raising a prompt that wants an answer,
//! - **echo** — the text showed up in the transcript (a fast answer, or a
//!   message queued behind a running turn),
//! - **changed** — not an agent TUI (a shell), but the screen moved,
//! - **resubmit** — the paste sat unsent in the composer; ctl pressed Enter.
//!
//! None of those within the window → the paste is re-sent `--retry` times
//! as raw bytes (no new task), then ctl fails loudly. Only reads and raw
//! writes the daemon already serves, so it works against a daemon that
//! predates it.

use std::time::{Duration, Instant};

use seance_core::agent_state::{self, Activity};

use crate::control::ControlRequest;

use super::parse::{base64_encode, with_identity};
use super::send_request;

/// Client-side flags shared by `send` and `note-agent`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DeliverOpts {
    pub confirm: bool,
    /// note-agent: if Claude queues the note behind a running turn, press its
    /// "ctrl+x ctrl+s to send now". That INTERRUPTS the turn (the in-flight
    /// tool call is cancelled and the agent stops after answering), so it is
    /// never implied — `--interrupt` only.
    pub interrupt: bool,
    /// send: a busy pane gets the task queued daemon-side, not refused.
    pub queue: bool,
    pub retries: u32,
    pub window: Duration,
}

impl Default for DeliverOpts {
    fn default() -> Self {
        Self {
            confirm: true,
            interrupt: false,
            queue: false,
            retries: 1,
            window: Duration::from_secs(15),
        }
    }
}

/// Pull `--no-confirm`, `--retry N`, `--confirm-secs N` out of `args`.
pub(super) fn take_deliver_flags(args: &mut Vec<String>) -> Result<DeliverOpts, String> {
    let mut opts = DeliverOpts::default();
    let mut rest = Vec::with_capacity(args.len());
    let mut it = std::mem::take(args).into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-confirm" => opts.confirm = false,
            "--interrupt" => opts.interrupt = true,
            "--queue" => opts.queue = true,
            "--retry" => {
                opts.retries = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--retry needs a count")?
            }
            "--confirm-secs" => {
                opts.window = Duration::from_secs(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--confirm-secs needs seconds")?,
                )
            }
            _ => rest.push(a),
        }
    }
    *args = rest;
    Ok(opts)
}

/// One look at a pane (screen + verdict); the judge is shared with the
/// daemon's send queue.
pub(super) use seance_core::agent_state::{judge, Judgement, Look as Probe};

pub(super) fn probe(pane: &str, scope: &Option<String>, from: &Option<String>) -> Option<Probe> {
    let ask = |req| send_request(&with_identity(req, scope.clone(), from.clone())).ok();
    let read = ask(ControlRequest::Read {
        pane: pane.to_string(),
        lines: None,
        scope: None,
        from: None,
    })?;
    if !read.ok {
        return None;
    }
    let screen = read
        .data
        .as_ref()
        .and_then(|d| d.get("screen"))
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let title = ask(ControlRequest::Status {
        pane: pane.to_string(),
        scope: None,
        from: None,
    })
    .and_then(|r| r.data)
    .and_then(|d| d.get("title").and_then(|t| t.as_str()).map(String::from));
    Some(Probe::of(screen, title.as_deref()))
}

/// Bracketed paste + (optionally) Enter as raw PTY bytes: the delivery path
/// that opens **no task**. Same shape and settle as the daemon's inject.
pub(super) fn paste_raw(
    pane: &str,
    text: &str,
    submit: bool,
    scope: &Option<String>,
    from: &Option<String>,
) -> Result<(), String> {
    let mut bytes = b"\x1b[200~".to_vec();
    bytes.extend_from_slice(text.replace('\x1b', "").as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    raw(pane, &bytes, scope, from)?;
    if submit {
        std::thread::sleep(Duration::from_millis(180));
        raw(pane, b"\r", scope, from)?;
        if text.contains('\n') {
            std::thread::sleep(Duration::from_millis(80));
            raw(pane, b"\r", scope, from)?;
        }
    }
    Ok(())
}

pub(super) fn raw(
    pane: &str,
    bytes: &[u8],
    scope: &Option<String>,
    from: &Option<String>,
) -> Result<(), String> {
    let req = with_identity(
        ControlRequest::SendRaw {
            pane: pane.to_string(),
            bytes_b64: base64_encode(bytes),
            force: false,
            scope: None,
            from: None,
        },
        scope.clone(),
        from.clone(),
    );
    match send_request(&req) {
        Ok(r) if r.ok => Ok(()),
        Ok(r) => Err(r.error.unwrap_or_else(|| "send-raw failed".into())),
        Err(_) => Err("cannot reach the daemon".into()),
    }
}

/// Result of [`confirm`]: how it landed, and after how many writes.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Delivery {
    pub via: &'static str,
    pub attempts: u32,
}

/// Watch `pane` after a write of `text` until [`judge`] says delivered.
/// Re-pastes (raw, no task) up to `opts.retries` times. `Err` carries a
/// message naming what the screen showed last.
pub(super) fn confirm(
    pane: &str,
    text: &str,
    before: &Probe,
    opts: &DeliverOpts,
    scope: &Option<String>,
    from: &Option<String>,
) -> Result<Delivery, String> {
    let mut attempts = 1;
    loop {
        let started = Instant::now();
        let mut stuck_since: Option<Instant> = None;
        let mut resubmits = 0;
        let mut last = before.clone();
        while started.elapsed() < opts.window {
            std::thread::sleep(Duration::from_millis(300));
            let Some(now) = probe(pane, scope, from) else {
                continue;
            };
            match judge(before, &now, text) {
                Judgement::Delivered(via) => return Ok(Delivery { via, attempts }),
                Judgement::Stuck => {
                    // Give the TUI a beat to take the paste before pressing
                    // Enter for it; twice at most per attempt.
                    let since = *stuck_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_millis(1200) && resubmits < 2 {
                        raw(pane, b"\r", scope, from)?;
                        resubmits += 1;
                        stuck_since = None;
                    }
                }
                // Enter cleared the composer: the TUI took the text even if
                // it hasn't visibly started. Re-pasting would duplicate it.
                Judgement::Pending if resubmits > 0 => {
                    return Ok(Delivery {
                        via: "resubmit",
                        attempts,
                    })
                }
                Judgement::Pending => stuck_since = None,
            }
            last = now;
        }
        // A busy agent takes input mid-turn without echoing it (Claude folds
        // it into the running turn). Re-pasting there duplicates the text —
        // what queued five copies into v2-simp-ios on 2026-09-28.
        if before.activity == Activity::Busy && !agent_state::composer_holds(&last.screen, text) {
            return Ok(Delivery {
                via: "mid-turn",
                attempts,
            });
        }
        if attempts > opts.retries {
            return Err(format!(
                "not confirmed delivered to '{pane}' after {attempts} attempt(s) \
                 ({}s each): pane reads {}{}",
                opts.window.as_secs(),
                last.activity.as_str(),
                last.evidence
                    .as_deref()
                    .map(|e| format!(" — \"{e}\""))
                    .unwrap_or_default()
            ));
        }
        attempts += 1;
        paste_raw(pane, text, true, scope, from)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(screen: &str) -> Probe {
        Probe::of(screen.to_string(), None)
    }

    const IDLE: &str = "✻ Baked for 7m 48s\n\n────\n❯\n────\n  [account-1] 5h:5% 7d:1%";
    const BUSY: &str = "✶ Osmosing… (3s · ↓ 1k tokens)\n\n────\n❯\n────\n  [account-1] 5h:5% 7d:1%";

    #[test]
    fn deliver_flags_parse_and_strip() {
        let mut args: Vec<String> = [
            "w",
            "--retry",
            "3",
            "hi",
            "--no-confirm",
            "--confirm-secs",
            "5",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = take_deliver_flags(&mut args).unwrap();
        assert_eq!(args, vec!["w".to_string(), "hi".to_string()]);
        assert!(!o.confirm);
        assert_eq!(o.retries, 3);
        assert_eq!(o.window, Duration::from_secs(5));
        let mut bad = vec!["--retry".to_string()];
        assert!(take_deliver_flags(&mut bad).is_err());
    }

    #[test]
    fn idle_to_busy_is_delivered() {
        assert_eq!(
            judge(&p(IDLE), &p(BUSY), "do the thing"),
            Judgement::Delivered("busy")
        );
    }

    #[test]
    fn nothing_changed_is_pending_so_retry_can_fire() {
        // The 9-27 failure: paste vanished, prompt still empty.
        assert_eq!(
            judge(&p(IDLE), &p(IDLE), "do the thing"),
            Judgement::Pending
        );
    }

    #[test]
    fn paste_left_in_composer_is_stuck() {
        let stuck = IDLE.replace("❯\n", "❯ do the thing now please\n");
        assert_eq!(
            judge(&p(IDLE), &p(&stuck), "do the thing now please"),
            Judgement::Stuck
        );
    }

    #[test]
    fn echo_counts_only_new_occurrences() {
        let echoed = format!("> do the thing\n  now please\n{IDLE}");
        let text = "do the thing now please";
        assert_eq!(
            judge(&p(IDLE), &p(&echoed), text),
            Judgement::Delivered("echo")
        );
        // Already on screen before (a re-send of the same task): not proof.
        assert_eq!(judge(&p(&echoed), &p(&echoed), text), Judgement::Pending);
    }

    #[test]
    fn busy_before_needs_echo_not_spinner() {
        assert_eq!(
            judge(&p(BUSY), &p(BUSY), "note: rebase first"),
            Judgement::Pending
        );
        let queued = BUSY.replace("❯\n", "❯\n  ↳ note: rebase first\n");
        assert_eq!(
            judge(&p(BUSY), &p(&queued), "note: rebase first"),
            Judgement::Delivered("echo")
        );
    }

    #[test]
    fn shell_screen_change_counts_and_modal_counts() {
        assert_eq!(
            judge(&p("$ "), &p("$ ls\nfoo"), "ls"),
            Judgement::Delivered("changed")
        );
        let modal = " Do you want to proceed?\n ❯ 1. Yes\n   2. No";
        assert_eq!(
            judge(&p(IDLE), &p(modal), "rm x"),
            Judgement::Delivered("modal")
        );
    }
}
