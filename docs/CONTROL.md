# seance — control plane

The control plane is how **anyone in the circle** (agent, shell script, or
human at a keyboard outside the GUI) engages the shared space.

A pane — any agent CLI or shell — can spawn siblings, type into them, read
their screens, ask the human a blocking question, or propose a command as
ghost text. The human watches every terminal live. Visibility is the point;
the protocol exists so agents don’t work offstage.

The technique for injecting prompts is borrowed from the `decap` project:
**bracketed paste** (`\x1b[200~` … `\x1b[201~`) followed by a carriage return to
submit. TUI agents treat a bracketed-paste block as one atomic paste, so a
multi-line prompt lands intact instead of triggering per-line submits.

Two surfaces expose the same protocol:

- a **Unix-socket server** the daemon runs (`src/daemon/` +
  `Engine::handle_control`; wire types in `crates/seance-core/src/control.rs`),
- a **CLI client**, `seance ctl …` (`src/ctl/`), which a master session
  shells out to.

---

## Transport

Outside a pane, `seance ctl` follows the app's saved remote host automatically.
Explicit `SEANCE_SOCKET` or in-pane `SEANCE_SESSION` keeps the direct-socket
path; `seance ctl --local …` bypasses the preference. The proxy runs the host
CLI so scratchpads and agent profiles resolve there. Prompt `--file` inputs
are read on the client; viewer/cwd/artifact paths refer to the host.
See [REMOTE.md](REMOTE.md#cli-files).

- **Socket:** `$XDG_RUNTIME_DIR/seance.sock`, falling back to
  `/tmp/seance-$UID.sock` when `XDG_RUNTIME_DIR` is unset. `$SEANCE_SOCKET`
  overrides both (it is exported into every pane).
- **Wire format:** **JSON lines.** One request JSON object per `\n`-terminated
  line in; one response JSON object per line out. A connection may stay open and
  carry many request/response pairs in order — a master session can keep one
  socket and pipeline `send`/`read` without reconnecting.
- **Concurrency:** the server accepts on one thread and handles each connection
  on its own thread (blocking IO, no async runtime). Each request is answered
  under the engine mutex — there is no timeout, so a wedged engine blocks the
  caller rather than returning an error.
- **Single instance:** on startup the server try-connects to an existing socket
  file. If a live server answers, it refuses to start (two seances would fight
  over the socket); a stale file left by a crash is removed and rebound.

### Request

Every request is a JSON object tagged by an `op` field (snake_case):

| op | fields | meaning |
|----|--------|---------|
| `list` | — | list every session with status |
| `new` | `name` (req), `cwd?`, `command?`, `workspace?` | spawn a session |
| `send` | `session`, `text`, `submit?`=`true` | paste `text`, optionally submit |
| `send_raw` | `session`, `bytes_b64` | inject raw bytes (base64), no wrap, no submit |
| `read` | `session`, `lines?` | rendered visible screen (tail N lines if given) |
| `status` | `session` | one session's metadata |
| `kill` | `session` | terminate the session |
| `scratchpad` | `session` | path to the session's shared scratchpad file |

`session` is a session name/slug (the app resolves either).

### Response

```json
{ "ok": true,  "data": <any> }
{ "ok": false, "error": "human-readable message" }
```

`data` is omitted on bare successes (e.g. `kill`); `error` is omitted on
success. Response payloads by op (best-effort shapes the app fills in):

| op | `data` on success |
|----|-------------------|
| `list` | `{panes: [...], scope, event_seq, focused_pane, selected_workspace}` |
| `new` | `{slug, name, workspace, scratchpad, command, cwd, parent}` |
| `send` | `{slug, task_id, inject_pad_rev, inject_pad_bytes, status}` |
| `send_raw` | `null` |
| `read` | `{screen: "..."}` |
| `status` | the pane's roster row: `{slug, name, kind, workspace, cwd, command, parent, running, activity, activity_evidence, quota, title, status, task_id, …}` |
| `kill` | omitted / `null` |
| `scratchpad` | `{path: "..."}` |

---

## Workspace scoping (v0.3)

Every op carries an optional `scope` field naming a workspace. When set, the
op only sees/affects panes in that workspace:

- `list` filters to the scoped workspace; the response echoes `"scope"`.
- `new` spawns into the scoped workspace by default and refuses `--workspace`
  values outside it.
- All pane-targeting ops (`send`/`send-raw`/`read`/`status`/`kill`/
  `scratchpad`) error with a clear message if the pane is outside scope.

**The CLI fills `scope` automatically from `$SEANCE_WORKSPACE`** — so a ctl
run *inside* a seance pane is confined to its own workspace. A master agent
in workspace `lab` can only drive `lab` panes. Overrides: `--all` lifts the
scope for one call; `--scope WS` targets another workspace explicitly.
Callers outside seance (no env var) are unscoped and see everything.

### Circle identity: slug vs label (0.16+)

A circle has a **slug** — its identity, minted once from the name it was
created with — and a **label**, the mutable text you read. This is the same
split panes have always had (`slug` / `name`), and it exists for the same
reason: a rename must not move an identity that other things are holding.

`$SEANCE_WORKSPACE` is the **slug**, so it stays true for the life of the
pane. Under the old model the display name *was* the key, so renaming a circle
orphaned every holder of the old string — including the environment of every
process already running in it, which nothing can reach.

`scope` / `workspace` accept **either form**, resolved at the daemon door on
both the control and GUI planes: an exact slug wins, then an unambiguous
label. A label two circles share resolves to neither and errors, rather than
picking one. A key matching nothing is left as-is, so it goes on matching no
circle instead of being silently promoted to unscoped.

**`seance ctl whoami` is the authority on which circle you are in** — it
answers from the pane, not from the caller's environment.

Naming note: panes were called "sessions" in v0.1; the wire protocol accepts
both `pane` and `session` keys for pane ids. Terminals are the first pane
kind (`"kind": "terminal"` in `list`/`status`) — the protocol is
kind-agnostic so markdown/graph/etc panes can land without breaking clients.

## v0.7 additions (the transparency release)

- Every op now carries `from` (the calling pane's slug, auto-filled from
  `$SEANCE_SESSION`) — actions are **attributed**: `human` / `agent:<pane>` /
  `cli` in one event log at `~/.local/share/seance/events.jsonl`. (`human`
  never comes from the control plane — only from GUI input.)
- `timeline` — query the log (`since_secs`, `pane`, `actor`, `limit`).
- `status_set` — agent self-reported status; badge on the pane + colored
  sidebar dot (planning|working|blocked|needs-human|done|idle).
- `ask` / `ask_result` — agent asks the human; a toast with choice buttons
  appears above the tiling region; the CLI blocks and prints the answer.
- Cross-pane `send`/`read` flash "⚡ driven by X" / "👁 observed by X" on the
  target pane strip for ~5s. The activity drawer (`≋` in the footer) shows
  the live event feed.

## Semantics

### `send` — the driving primitive

`send` with `submit: true` (the default):

1. the app **bracketed-pastes** `text` into the session's terminal
   (`\x1b[200~` + text + `\x1b[201~`) — the paste wrapping happens app-side, you
   just supply the raw text,
2. waits a **180 ms settle delay**, then
3. sends a **carriage return** (`\r`) to submit.

The settle delay matters: TUI agents (claude / codex / grok CLIs) need a beat to
finish processing the paste before the Enter keystroke lands, or the submit
races the paste and gets eaten. With `submit: false` the text is left sitting in
the input unsent — useful for staging a prompt you'll submit later, or for
composing across several `send` calls.

### `send_raw` — the escape hatch

`send_raw` writes raw bytes straight to the PTY: no paste wrapping, no settle
delay, no submit. It's for control characters and key sequences the paste path
can't express — Ctrl-C to interrupt a running command (`0x03`), a bare Enter
(`0x0d`), arrow keys, Escape. Bytes are base64-encoded on the wire; the CLI
encodes for you (pass a shell-escaped string).

### `read` — the observing primitive

`read` returns the **rendered visible screen** — the exact text a human sees in
the pane, not the raw PTY byte stream. With `lines: N` it returns the tail N
lines (reaching into scrollback as needed); omitted, it returns the full visible
screen. This is how a master session watches a worker: poll `read` until the
screen shows the worker is idle / awaiting input, then act.

Because `read` is a screen snapshot, expect prompt boxes, spinners, and partial
frames. Poll on an interval and look for a stable idle marker (an empty prompt,
a `>` cursor, "esc to interrupt" gone) rather than parsing mid-render frames.

### `scratchpad` — the durable side-channel

Each session has a markdown scratchpad at
`~/.local/share/seance/scratch/<slug>.md`, surfaced live in the app's drawer.
The agent *inside* a session sees its own path via `$SEANCE_SCRATCHPAD`. A
master reads/writes a worker's scratchpad by path (from `scratchpad`) to hand
off durable instructions and collect durable results — notes that outlive any
single screen and that the human can watch update in real time.

### Session environment

Spawned sessions get these env vars so the agent inside knows it's under seance:

- `SEANCE_SESSION` — the pane's human-readable slug (can be reused after deletion),
- `SEANCE_PANE_ID` — the pane's lifetime UUIDv4, never reused,
- `SEANCE_WORKSPACE_ID` — its circle's lifetime UUIDv4, never reused,
- `SEANCE_WORKSPACE` — its circle's **slug** (what makes scoping automatic).
  Stable for the pane's whole life: renaming a circle changes its label, never
  this,
- `SEANCE_WORKSPACE_NAME` — the circle's current label, for display only. This
  one *can* go stale after a rename, which is exactly why nothing addresses by
  it,
- `SEANCE_SCRATCHPAD` — absolute path to its own scratchpad file,
- `SEANCE_SOCKET` — the control socket to talk back on.

Use `pane_id` and `workspace_id` for durable external mappings (for example,
AFK topics). Both appear on every pane row in `ctl list --all --json`,
`roster --json`, `brief --json`, and `status PANE --json`, and in
`ctl whoami --json`. Empty circles also have IDs, stored in state even when
they have no panes. Slugs remain the human-readable addresses; IDs are
independent of slugs, labels, agent conversation IDs, and process PIDs.

Any ctl pane target accepts `id:<pane_id>`, including `wait`, `handoff`,
`roster --parent`, and cross-circle message recipients. For example:
`seance ctl status "id:$SEANCE_PANE_ID" --json`. Workspace scoping still
applies: pass `--all` when intentionally addressing another circle.
An unknown or deleted ID is an error and never falls back to a reused slug.

Pane UUIDs are saved in `state.json` under `sessions[].pane_id`; circle
UUIDs are saved in the `workspace_ids` map keyed by circle slug. Cold
restore, sleep/wake, and daemon upgrade transfer these same values. Renames
change labels only. Killing a pane and creating another, including a
`ctl handoff` successor, mints a new pane UUID. Deleting and recreating a
circle likewise mints a new workspace UUID. On first load of legacy state,
missing IDs are minted and the state is saved immediately, before restore
launches any process; failure to save aborts startup.

Deployment: build with `cargo build --release`, then run `seance upgrade`
when ready. The new daemon migrates legacy upgrade bundles and saves their
IDs immediately. A live upgrade preserves existing process environments:
those panes expose IDs through ctl immediately, but gain the new env vars
only after sleep/wake or a cold restore relaunches their processes. A GUI
restart alone does not deploy this change. If a pane is moved to another
circle, ctl reports its current circle ID; its process environment retains
the launch-time circle until relaunch.

Seance adds `--no-daemon` to direct `codex` launches when the installed Codex
supports it. Shared Codex app-servers can retain another pane's
`SEANCE_SESSION`; identity-bound `ctl` calls from that process tree are
rejected. Existing Codex sessions using a shared app-server need relaunching
with `--no-daemon`. `ctl` also rejects an environment identity that differs
from the original pane process, or whose ancestry cannot be verified.
Launch isolation and task validation run in the daemon:
deploy their updates with `seance upgrade`, not a GUI restart.

---

## CLI reference — `seance ctl`

```
seance ctl <command> [args] [--json]
```

`--json` (accepted anywhere) prints the raw JSON response instead of the
human-readable rendering. Exit codes: **0** ok · **1** request failed · **2**
cannot connect (with an "is seance running?" hint) · **3** `send`/`note-agent`
accepted but delivery not confirmed.

| command | usage |
|---------|-------|
| `list` | `seance ctl list` |
| `new` | `seance ctl new --name NAME [--cwd DIR] [--agent NAME\|--command CMD] [--workspace WS] [--wait-ready] [--task-file PATH]` |
| `send` | `seance ctl send PANE TEXT...` `[--file PATH\|--stdin] [--no-submit] [--force] [--retry N] [--no-confirm]` → `task_id` + `delivery` |
| `note-agent` | `seance ctl note-agent PANE TEXT...` `[--file\|--stdin]` — into the current task, no new task |
| `handoff` | `seance ctl handoff PANE [--agent claude] [--name N] [--note T] [--keep]` |
| `ask --to` | `seance ctl ask --to CIRCLE TEXT` `[--file\|--stdin] [--no-wait] [--timeout S]` → answer on stdout (docs/COMMS.md) |
| `tell` / `reply` / `await` | `tell CIRCLE TEXT` · `reply ID TEXT` · `await ID` |
| `messages` / `lead` / `contacts` | `messages [--circle C\|--pane P]` · `lead [CIRCLE] [--set PANE]` · `contacts [PANE]` |
| `send-raw` | `seance ctl send-raw PANE BYTES` |
| `read` | `seance ctl read PANE [--lines N]` (debug) |
| `status` / `kill` | `seance ctl status\|kill PANE` |
| `rename-pane` | `seance ctl rename-pane [PANE] NAME`: set a pane's display name (default: your own pane); the slug ctl addresses never changes, and an empty name is refused |
| `sleep` / `wake` | `seance ctl sleep [CIRCLE]` · `seance ctl wake [CIRCLE]` (alias `awaken`): put a whole circle to sleep or wake it; omit CIRCLE inside a pane for your own. Sleep is refused when a pane in the circle can't be restored |
| `scratchpad` / `pad` | `seance ctl pad [PANE] [--cat]` (default: `$SEANCE_SESSION`) |
| `note` | `seance ctl note [PANE] TEXT...` `[--file PATH] [--replace]` |
| `finish` | `seance ctl finish [PANE] [--file\|--stdin] [--status done] [--note N] [--task ID]` |
| `task` / `inbox` | durable inject body (`--id` or self) |
| `roster` / `stage` / `brief` | dense stage projection |
| `wait` | `wait PANE… --status done [--cat\|--harvest] [--badge-only] …` |
| `harvest` | alias: `wait … --status done --cat` |
| `whoami` | principal + session + active `task_id` |
| `doctor` / `skill` / `help` | profiles + agent contract |
| `space` | `seance ctl space [list]` · `space new NAME` · `space rename SPACE NAME` · `space rm SPACE` · `space default SPACE` — the rail's spaces (0.27) |
| `move` | `seance ctl move [CIRCLE] SPACE` — put a circle in a space; omit CIRCLE inside a pane for your own |
| `layout` | `seance ctl layout` lists the window layouts (classic · stage · chat) and each attached window's current one · `seance ctl layout NAME` switches every attached window (each GUI then saves it locally; nothing is stored daemon-side); an unknown NAME is refused with the list |
| `theme` | `seance ctl theme` lists the color schemes (active starred) · `seance ctl theme ID` switches every attached window and saves the choice daemon-side; an unknown ID is refused with the list |

Notes:

- **`send`**: shell expands `$VARS` — use `--file`/`--stdin`. Inject creates a
  **task envelope** (`task_id`), sets `status=working`, records pad baseline.
  A send never cancels the pane's task unless asked. On an idle pane the new
  task goes in now and the open one becomes `parked` (its turn was over: it
  can still be finished with `ctl finish --task ID`, shows in the row's
  `parked_tasks`, and prunes after 7 days like a done task). On a busy pane,
  or one with tasks already waiting, it is queued (see below). `--now`
  (`{"now":true}`) cancels the open task instead, on every path, and a
  queued `--now` send goes to the front of the queue. For
  coordinator notes, use `note-agent` (no task creation); otherwise track
  the new task id. Sidecars: `<scratch>.taskid` /
  `<scratch>.task.json`.
- **Delivery (`send`, `note-agent`)**: after the write, ctl watches the pane
  until the agent goes busy, the text echoes in the transcript, or the paste
  sits unsent in the composer (then ctl presses Enter). If nothing happens
  within `--confirm-secs` (15), it re-pastes as raw bytes, which opens no new
  task, up to `--retry` (1) times. If delivery still isn't confirmed it
  exits **3**, and `--json` carries the open `task_id`. A pane showing a modal
  is refused rather than pasted into.
- **`activity`** (roster/brief/status, daemon-side): `busy` · `idle` ·
  `awaiting-input` · `limited` · `exited` · `unknown`, plus
  `activity_evidence` (the deciding screen line) and `quota`. The classifier
  lives in `seance-core/src/agent_state.rs` and is pinned by real frames.
- **Busy panes (`send`)**: ctl looks before sending. A `busy` pane, or any
  `--queue` send, goes as `{"op":"send","queue":true}` (the daemon also
  queues a plain send to a pane it reads busy): the daemon records the task
  as `queued`, returns `{task_id, status: "queued", position, behind,
  ahead}` (printed `queued behind task-N (K ahead)`), and its queue pump
  (`engine/queue.rs`, every 500ms) injects it after the pane has read
  `idle` for 1.5s straight, oldest first, parking any task still open, then
  confirms it with the shared judge (Enter re-pressed, one re-paste, else
  `failed`).
- **`task_fail`** `{id, reason?}`: mark an open/queued task `failed`. If
  injecting it parked or cancelled the pane's previous open task, that task
  is reopened (`restored` in the reply). ctl calls it when a send's delivery
  can't be confirmed. Capability: `send`.
- **Queued input (`note-agent`)**: a busy Claude parks pasted text behind the
  running turn ("Press up to edit queued messages"). That counts as delivered
  (`via: "queued"`, row `queued_input: true`), and ctl never re-pastes into a
  busy pane. `--interrupt` presses Claude's ctrl+x ctrl+s ("send now"), which
  interrupts the turn and cancels the in-flight tool call.
- **`wait --artifact PATH`**: the file exists with ≥ `--min-bytes`.
  `--artifact-match REGEX` / `--artifact-contains TEXT` also require the body
  to match (dot matches newlines).
  `--fresh` requires it to be written after the `--task` was sent (or after
  the wait began). `--either` ORs it with the other conditions, which is the
  robust completion pattern. `wait` exits 1 at once if the pane is gone or
  exited, or if `--task` was superseded, unless the artifact is already there.
- **`wait --status done`**: evidence-bound (pad must grow since inject) unless
  `--badge-only`. Prints `done …` (not `ready`) when waiting on done.
  `--task ID` waits for that exact task; a cancelled task times out even if
  the worker completes a newer task.
- **`--cat` / `harvest`**: after success, print each pane's pad body (fan-in).
- **`finish`**: pad body + status + task close; `done` requires body.
  `--task ID` must name the pane's active task or one of its parked tasks
  (finishing a parked one leaves the pane's status badge and current task
  alone; its body still goes into the pane's scratchpad, so use the default
  append, not `--replace`). Unknown, foreign-pane, cancelled or completed
  ids are rejected before changing the scratchpad or status.
- **Roster** prefers **slug** when name≠slug (ctl needs slug).
- **Sleeping panes**: `send` to a sleeping pane wakes it
  into the same conversation, then delivers once it reads idle; queued tasks
  and messages wake it the same way. The daemon also sleeps idle Claude
  panes one at a time on its own (`seance-auto-sleep-idle`, default `12h`,
  `off` to stop; `seance-auto-sleep-skip` lists circles to leave alone, both
  in `terminal.conf`), never one that is busy, waiting on a human or
  focused. Queued work doesn't keep a pane awake: the pump wakes it to
  deliver.

### Examples

```bash
seance ctl new --name build --cwd ~/proj --agent claude --wait-ready
seance ctl send build --file /tmp/task.md          # task=task-N
seance ctl wait build --status done --timeout 600 --cat
seance ctl harvest w1 w2 w3 --timeout 900
seance ctl task                                    # inside a worker
seance ctl finish --stdin --status done <<'EOF'
answer
EOF
seance ctl roster
```

---

## Agent skill (engagement protocol)

**Canonical source: `seance ctl skill`** — ships in the binary so it cannot
drift from the implementation. That text is the product contract for how
agents engage the human and the circle (status, ask, propose, scratchpad,
read-before-assume). Prefer the command over any doc snapshot.

Three ways to arm an agent (any CLI with shell access — not vendor-specific):

1. Tell it: *run `seance ctl skill` and follow those instructions*
2. Inject: `seance ctl send PANE "$(seance ctl skill)"` (or the ⚡ arm control)
3. Paste the output into whatever system/context file that agent reads
4. **Never arm by hand** — load it at session start, gated on `$SEANCE_SESSION`
   so sessions outside seance are untouched:
   - Claude Code: a `SessionStart` hook (matcher `startup|resume|clear|compact`,
     so it survives `/clear` and compaction) in `~/.claude/settings.json` that
     prints `seance ctl whoami` + `seance ctl skill` when `$SEANCE_SESSION` is
     set. Hook stdout becomes session context.
   - Codex: a gated paragraph in `~/.codex/AGENTS.md` telling it to run
     `seance ctl whoami` and `seance ctl skill` first when `SEANCE_SESSION` is
     set (Codex has no session-start hook).
   zack's setup: `vita/scripts/seance_session_context.sh`.

Default `new` pane command is a **shell** (human can always take the keyboard).
Pass `--command claude` / `codex` / `grok` / … for an agent worker.

---

## Foundation 0.9.1 — event bus, watch, capabilities

### Event bus

Every meaningful action publishes an `Event` with:

| field | meaning |
|-------|---------|
| `id` / `seq` | stable id + monotonic sequence |
| `actor` | `human` / `agent:<slug>` / `cli` / `daemon` / `system` |
| `kind` | machine-readable (`ctl_send`, `cmd_start`, `focus`, …) |
| `origin` | optional provenance (`ctl_send`, `human_keystroke`, `propose_accepted`, …) |
| `caused_by` / `span` | causal chain + span id (e.g. command runs) |

Durable: `~/.local/share/seance/events.jsonl`. Live: in-process subscribers.

### Replay attribution (0.11)

The daemon also records every pane into the replay ring (docs/REPLAY.md), and
control-plane traffic lands there attributed: `ctl send` writes a
`ReplayEvent::Send { from: "agent:<slug>" | "cli", text, submit }` with the
prompt verbatim, `ctl send-raw` writes `ReplayEvent::Input { origin:
"agent:<slug>" | "cli" }`. GUI human input is the only thing stamped `human`.
Caveat worth knowing when reading a replay: the player's **chapters** are built
from human keystroke lines *and* from any submitting `Send`, and `Chapter`
carries no attribution — so an agent's `ctl send` shows up in the prompt rail
looking exactly like a human prompt.

### `seance ctl watch`

```bash
seance ctl watch --kinds status_set,ask,cmd_end
seance ctl watch --pane worker-1 --since-seq 42
seance ctl watch --no-catch-up   # live only
```

After the ack line, the connection streams `ControlResponse` lines whose
`data` is a full event object until the client disconnects. This is the
epoll of the collab OS — prefer it over polling `read`.

### Capabilities / policy

Default policy is **`open`** (legacy behaviour). Persist at
`~/.local/share/seance/caps.json`.

```bash
seance ctl policy                          # get
seance ctl policy set propose_required     # send/send_raw need grant or propose
seance ctl policy set locked               # mutating ops denied without grant
seance ctl policy set open
seance ctl grant agent:worker-1 send --ttl 3600
seance ctl revoke agent:worker-1 send
seance ctl whoami
seance ctl caps
```

Human UI and daemon are always unrestricted. Always-free ops under any policy:
list/read/status/timeline/watch/human/ask/propose/status_set/scratchpad/commands/…

### Causal attribution tint

PTY stdin is tagged with origin. The GUI paints a 2px left gutter:

- faint — last input was human
- violet — agent / cli inject
- flame — accepted ghost proposal

`seance-origin-gutter = false` in terminal.conf turns the gutter off.

### Docs vs skill

**Canonical agent contract is `seance ctl skill`.** This file and `ctl help`
track the surface; when they disagree, skill + this foundation section win.
