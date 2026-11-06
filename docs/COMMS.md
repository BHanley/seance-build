# Sessions talking to each other

How one session (an agent in a pane, or a script) asks, tells and answers
another, by the names the human uses. Code: `src/runtime/engine/comms.rs`
(daemon), `src/ctl/comms.rs` (verbs), delivery in `src/runtime/engine/queue.rs`.

## Why

2026-09-29: paceline-desk asked paceline-arch-impl a question and wrote
*"reply via `seance ctl send --all claude-27`"*. `claude-27` was desk's
**circle** slug; `ctl send` resolves **panes**, and the pane named `claude-27`
was cadence-slack's. The answer landed in the wrong session. The earlier FYIs
were `send`s too, so each opened a task and cancelled the one before it.

Four problems, four fixes:

| problem | fix |
|---|---|
| circles and panes share auto-names (`claude-N`) | a bare name that is both a pane and another circle is **refused**; say `@name` (circle) or `pane:name` |
| the human says labels; agents need pane slugs | address **circles by label**; a circle is answered by its **lead** |
| replies are hand-addressed | `reply m-N` routes back **by message id** |
| questions ride the task channel | messages are **not tasks**: they never open or cancel one |

## Verbs

```bash
seance ctl ask --to paceline-arch-impl "does v2 touch group_commands.rb?"   # blocks, prints the answer
seance ctl ask --to paceline-arch-impl --no-wait --file q.md               # returns m-N now
seance ctl await m-12 [--timeout S]                                         # fetch / wait for an answer
seance ctl tell paceline-desk "FYI: #23997 carries the guard"              # one-way
seance ctl reply m-12 <<'EOF'                                               # answer (stdin read when piped)
pick (a)
EOF
seance ctl messages [--circle C | --pane P] [--limit N]
seance ctl lead [CIRCLE] [--set PANE]
seance ctl contacts [PANE]
```

Plain `seance ctl ask "Q" --choices a,b` (no `--to`) is still the blocking
question to the **human**.

Exit codes: `ask`/`await` print the answer and exit 0; **5** = no answer
within `--timeout` (default 600s) — the answer is still delivered later;
1 = could not be delivered (e.g. the recipient is a plain shell).

## Addressing

| form | means |
|---|---|
| `paceline-arch-impl` | circle by current label → its lead |
| `claude-47` | circle by slug → its lead |
| an old label | the circle that had it (warns "was renamed to …"), unless another circle holds that label now |
| `@name` | circle only |
| `pane:slug` | exactly that pane |
| bare `name` that is both a pane and another circle | **error**: say which |

The same `@name` / `pane:name` forms and the same ambiguity check apply to
every pane verb (`send`, `note-agent`, `read`, `status`, …) when unscoped.

**Lead** = `ctl lead CIRCLE --set PANE` if that pane is still in the circle,
else the circle's first-born terminal pane (panes older than the birth record
count as oldest, in pane-list order).

## Delivery

Messages go through the daemon's send-queue pump (every 500ms):

- pasted into the recipient's agent TUI with a header — `📨 seance m-12 —
  question from paceline-desk (pane claude-21): … Answer it with seance ctl
  reply m-12` — while it is **idle or busy** (a busy Claude queues it behind
  its turn; it is never interrupted), not while a modal is up, never over a
  human who is typing;
- confirmed with the same judge `ctl send` uses; never re-pasted into a busy
  pane (that stacks copies);
- a recipient that is not an agent session (plain shell) fails the message
  instead of typing it into a prompt.

A **reply** goes to whoever asked. If a `ctl ask`/`await` is blocked on it
(polled within the last 5s) it arrives on that command's stdout and nothing is
pasted; otherwise it is pasted into the asker's pane, so a timed-out asker
still gets it.

## Contacts and notices

A pane that addresses another circle (`ask`, `tell`, `reply`, `send`,
`note-agent`/`send-raw` across circles) becomes that circle's **contact**. A
contact gets a `📇 seance notice` when the circle is **renamed** (old label keeps
resolving) or its **lead closes** (who leads now, or that the circle closed).
Newer notices about the same circle replace undelivered older ones.

When a pane closes: pending messages addressed to its circle re-route to the
new lead; a question it received but never answered is **re-asked** of the new
lead; messages addressed to the pane itself fail; it is dropped from every
contact list.

## State

`CommsState` (runtime/protocol.rs) — leads, former labels, contacts, pane birth
times, the last 500 messages — persists in `state.json` and rides the
`seance upgrade` handoff.

## Every session knows this without arming

`seance ctl skill` carries the protocol ("Talking to another circle"). Load it
at session start (docs/CONTROL.md, "Agent skill").
