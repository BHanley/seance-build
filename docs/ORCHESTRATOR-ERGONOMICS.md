# Orchestrator ergonomics — running log

Requests from the paceline realtime-v2 orchestrator (pane claude-38 driving
~10 codex/claude workers in circle `codex-10`), 2026-09-27. Goal: driving
many agents from seance should need no screen-scraping and no workarounds.
Its dispatcher (`vita/…/2026-09-27-realtime-v2/dispatch_codex.py`) records
every workaround this batch replaces.

**Deploy model.** Two halves:

- **ctl-side** (the `seance ctl` client): works against the *running* daemon
  because it only composes ops the daemon already serves. It goes live the
  moment `target/release/seance` is rebuilt, since `~/.local/bin/seance`
  symlinks to it. No daemon restart is needed, but rebuilding changes `send`
  behaviour for any orchestrator mid-run (see the maintenance window notes).
- **daemon-side**: needs `cargo build --release && seance upgrade`, which
  keeps sessions alive.

Tested against an isolated daemon (`XDG_RUNTIME_DIR` + `SEANCE_STATE_DIR` +
`SEANCE_SOCKET` in a scratch dir, `target/debug/seance daemon`) with real
Claude and Codex panes. The live daemon and its panes were not touched.

## Batch 1 — 2026-09-27 (commit 9f5f444, undeployed)

| # | Request | Change | Side | Test |
|---|---------|--------|------|------|
| 1 | `send` silently not arriving | `send` confirms delivery: busy / echo / Enter re-pressed on an unsent composer / screen change (shells). Otherwise it re-pastes raw (no new task) `--retry N` (default 1) times, then **exit 3** with the open `task_id`. Refuses to paste into a modal. `--no-confirm`, `--confirm-secs S`. | ctl | `ctl::deliver` judge tests; live: Claude `delivered=busy`; a stty -echo pane gives exit 3 after 2 attempts |
| 2 | note to a working agent | `ctl note-agent PANE TEXT` (`nudge` alias): raw bracketed paste + Enter, confirmed like send. No task is opened or cancelled. | ctl | live: note delivered into running task-1, and task-1 stayed open |
| 3 | busy/idle scraping | `seance_core::agent_state::classify(screen, title)` → `activity` = busy · idle · awaiting-input · limited (+ `exited`/`unknown`) with `activity_evidence`, in roster/brief/status. It reads the **live bottom** of the screen, ignoring a human's scrollback. It finds the spinner up to 24 lines above the input box (Claude prints tips under it; a 12-line scrape misread two busy panes at 8:19pm), and the title spinner `◐◑` also counts. | daemon (ctl uses the same fn client-side for send) | 11 frame tests from live panes |
| 4 | usage limits invisible | `quota` {account, five_hour_used_pct, seven_day_used_pct, five_hour_left_pct, weekly_left_pct} parsed from Claude statusline / Codex footer + warnings. `limited` fires on "hit your (usage) limit / limit reached / out of credits" near the prompt, but not on "…% of your weekly limit left". | daemon | frame tests |
| 5 | manual handover | `ctl handoff PANE [--agent claude] [--name N] [--note T] [--keep]`: spawns a successor in the same cwd and circle, waits for boot, kills the original (one writer), then re-sends the open task with a TAKEOVER preamble and confirms. Prints `{old, slug, task_id, from_task, …}`. | ctl | live: w (task-1) → w-claude task-4, delivered=busy |
| 6 | `new` slug + JSON | `new --json` honours `--wait-ready`. `--task-file PATH` / `--task-stdin` sends a first task (and implies wait-ready). One JSON line: `{slug, name, requested_name, workspace, cwd, command, parent, scratchpad, ready, task_id, delivery}`. Human output warns when the name was taken. | ctl (+ daemon adds name/command/cwd/parent to `new`; ctl fills gaps on old daemons) | parse tests; live Claude + Codex |
| 7 | `status --json` has no cwd | `status` returns the full roster row (`cwd`, `activity`, `quota`, `task_id`, `parent`, …). `handoff` falls back to `brief` for cwd on old daemons. | daemon | engine test |
| 8 | completion | `wait PANE --task ID --artifact PATH --either --fresh`: file (≥min-bytes, written after the task was sent) **or** task done. `wait` now fails fast (exit 1) on a missing/exited pane or a superseded `--task` (these used to time out silently), unless the artifact is there. Documented as *the* robust path in `ctl skill`. | ctl | `artifact_ready` test; live: superseded → exit 1 at once, `--either` → 2s |
| 9 | `seance --help` hangs | `--help`/`-h` anywhere (except `ctl`/`web`/`replay`, which own theirs) prints top-level usage. Before this, `seance upgrade --help` **upgraded the daemon** and `restart-gui --help` restarted the GUI. (Bare `seance --help` was already fixed in 00588d5; the installed release binary predates that.) | ctl binary | `wants_top_help` test |
| 10 | helper panes clutter | `parent` = the spawning pane's `$SEANCE_SESSION` at `new`. It's persisted and survives upgrade handoff; dropped on kill. `roster --parent SLUG` / `roster --top` filter. | daemon (filter ctl) | engine test |

Also fixed on the way: `new --wait-ready`'s boot clear used a blind Codex
sequence (`2\r`). On Codex's *trust* dialog, option 2 is "No, quit". It never
actually fired, because `new` didn't return the command. Dialogs are now
answered from the screen (`agents::boot_dialog_answer`: trust gets Enter, the
update menu gets Skip). Permission prompts are never auto-answered.

## Batch 2 — item 11 (commit 4a96cc5, undeployed)

| # | Request | Change | Side | Test |
|---|---------|--------|------|------|
| 11 | untrusted cwd: ready took ~2 min, then the first send's Enter answered Claude's trust prompt and Claude exited (v2-go-touchup, events 58148-58156) | The real Claude 2.1.280 dialog opens with the cursor on **"❯ No, exit"**, so any Enter quits. `--wait-ready` now detects a trust dialog and **fails at once** (0.3s), naming it and leaving the pane at the prompt. `--trust` (on `new` and `handoff`) accepts it one checked step at a time: an arrow toward the "Yes" line, re-read the screen, and Enter only when the cursor is on "Yes". Codex update menus are skipped. `send` / `note-agent` refuse to paste into any modal (from batch 1). This also corrects batch 1, which answered trust with a bare Enter: fine on Codex, fatal on Claude. | ctl | `agents::trust_is_navigated_to_yes_never_blind_enter` + a classifier frame test, both from the live dialog; live: no-trust → exit 1 in 0.3s, send refused, `--trust` → idle in 3.6s |

## Batch 4 — 2026-09-29: sessions talk by name (commits 6547ca2, 49f2119)

Trigger: paceline-desk asked paceline-arch-impl a question and named
`claude-27` as the reply address. That was its own **circle** slug, but also
the **pane** slug of cadence-slack, so the answer went to cadence-slack. Full
design: docs/COMMS.md.

- `ask --to CIRCLE` / `tell` / `reply m-N` / `await` / `messages`: messages,
  not tasks. Delivered by the queue pump behind a busy turn, and replies are
  routed back by id.
- Circles are addressed by label, slug or former label, and answered by
  their **lead** (first pane; `ctl lead --set` overrides).
- **Contacts** get notices on rename and when a lead closes; old labels keep
  resolving; unanswered asks are re-asked of the new lead.
- A bare name that is both a pane and another circle is refused on every
  pane verb; `@name` / `pane:name` say which.
- No arming: a SessionStart hook (vita `scripts/seance_session_context.sh`)
  loads `seance ctl skill` into every Claude session in a pane, and
  `~/.codex/AGENTS.md` does the same for Codex. The skill's "Talking to
  another circle" section teaches all of the above.

Verified with two real Claude sessions on an isolated daemon. Told only "ask
the v2-impl session what 17×23 is", one ran `ask --to v2-impl`, the other
answered with `reply m-3`, and the answer came back on stdout. No tasks were
touched.

**Deployed 2026-09-29 ~09:12** (`seance upgrade` of `49f2119`, built from a
clean worktree so the uncommitted web/remote_term work stayed out). 63 panes
before and after. **Incident:** after the upgrade, `ctl` from every
pre-upgrade pane failed with "cannot verify original pane identity:
Permission denied". Pane processes had been reparented to `systemd --user`
(a subreaper) and the 31d49de identity walk read its environ. Fixed in
`1e46c46` (ctl-only, live once the binary was rebuilt, ~10 minutes of
outage). No dispatcher was running at the time. Not yet done:
`seance restart-gui`, which only picks up the thinner ⚡ arm prompt.

## Batch 3 — 2026-09-28 (commit 6ea2193, undeployed)

Batches 1–2 were deployed at ~6:55am 9-28 (`seance upgrade` from d06e5d7).

| # | Request | Change | Side | Test |
|---|---------|--------|------|------|
| 1 | `send` to a busy pane: opened task-284 (cancelling task-283) and then "failed" | Root cause: Claude folds mid-turn input into the running turn without echoing it, so confirm saw nothing and re-pasted (event log: two writes). Now `send` **looks first**: busy means **exit 4**, nothing sent, no task opened. `--queue` makes the daemon record a `queued` task, and its queue pump (`engine/queue.rs`, 500ms) injects it after 1.5s of stable idle and confirms with the shared judge (Enter re-pressed / one re-paste / else `failed`). An unconfirmed send is **rolled back** by the new `task_fail` op: it is marked `failed` and the task it cancelled is reopened (`supersedes` on the record). | daemon (queue, task_fail) + ctl | engine test; live: busy → exit 4 (task-10 untouched); `--queue` → task-9 delivered after idle; stty-echo pane → exit 3, task-12 failed, task-11 reopened |
| 2 | `wait --artifact` fires on the first write | `--artifact-match REGEX` / `--artifact-contains TEXT` (dot matches newlines). `wait --task` also follows a queued task, and counts a task that finished before a later one became current as done. | ctl | `artifact_match_waits_for_the_closing_block` |
| 3 | context headroom | `context_left_pct` from Claude "Context left until auto-compact: N%" / "N% until auto-compact" and Codex "N% context left". | daemon | frame tests (no live pane showed it today) |
| 4 | queued input | `queued_input: true` when Claude shows "Press up to edit queued messages" / "ctrl+x ctrl+s to send now". That placeholder no longer reads as typed text. **Count: not offered**, because the screen shows one hint however many notes are queued and a count would be a guess. `queued_tasks` lists the daemon's `--queue` tasks instead. | daemon | live frame test (v2-simp-rails) |
| 5 | roster JSON undocumented | `ctl skill` lists every row field, the task statuses and the exit codes. | ctl | — |
| 6 | note-agent to a Claude main agent waiting on a subagent "never delivered" | It **was** delivered: Claude queued it in the main composer (focus is not on the agents panel). The deployed judge didn't know the queued marker, so it re-pasted, and the event log shows 5×~1010-byte writes, so several copies are probably queued on v2-simp-ios. Now "queued" counts as delivered (`via: "queued"`), busy panes are never re-pasted, and `--interrupt` delivers at once via Claude's ctrl+x ctrl+s. **Verified live: that interrupts the turn**, cancelling the running tool call, and the agent answers the notes and stops. Use it only when that is what you want. | ctl + core | judge tests; live: 3 notes queued once each; `--interrupt` → read, turn interrupted |

### Needs a maintenance window (batch 3)

`cargo build --release && seance upgrade` for the daemon half: the queue
pump, `task_fail`, and the new row fields. ctl-only parts (busy refusal,
`--artifact-match`, no re-paste into busy panes) work on the deployed daemon
once the binary is rebuilt. `send --queue` against the old daemon refuses
with "needs `seance upgrade`" instead of sending now. A failed send against
the old daemon can't roll back and says so. New exit code **4** (busy) means
the dispatcher should treat 4 as "not sent; queue or wait".

### Needs a maintenance window (batches 1–2, done 9-28)

1. **`cargo build --release && seance upgrade`**: daemon half (activity,
   quota, status row, new-response fields, parent). Sessions survive, but
   the rule for this job is no upgrade mid-run.
2. **Rebuilding `target/release/seance` at all** swaps `seance ctl` under
   the running dispatcher. Nothing breaks on the wire (same version), but
   `send` then blocks up to ~30s confirming, and exits **3** when delivery is
   unconfirmed. `dispatch_codex.py`'s `ctl()` raises on any non-zero exit.
   Adopt it deliberately: either pass `--no-confirm` or handle exit 3 (the
   JSON still carries `task_id`).
3. Existing Codex panes on a shared app-server still need relaunching with
   `--no-daemon` (31d49de).

### What the dispatcher can drop once deployed

- `new_pane` regex on "created X" → `new --json` (`data.slug`).
- `confirm_started` + `nudge` → `send` confirms / `note-agent`.
- `idle()` screen scraping → `status --json` `data.activity`.
- `LIMIT_RE` scraping → `activity == "limited"` / `quota`.
- `migrate()` + TAKEOVER rewrite → `handoff PANE --agent claude --json`.
- result-file poll loop → `wait PANE --task T --artifact F --either --fresh`.

### Not done / why

- `note-agent` doesn't append the note to the task record (`ctl task` still
  shows only the original body). That needs a daemon op; the note is in the
  transcript and the event log (`ctl_send_raw`).
- No agent hooks. The classifier reads the screen the daemon already owns,
  which covers every agent without per-agent config. Hooks (Claude
  Stop/SessionStart) would be a sturdier busy signal later.
