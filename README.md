# Seance, Mac client build

**Built on [zackham/seance](https://github.com/zackham/seance).** Seance is
Zack Ham's project: the daemon, the terminals, the control plane, the browser
client and most of what is below this section are his. This repository is a
build of it that adds a few screens on top. It is not upstream, and it is not
supported by upstream. MIT license, as upstream (see `LICENSE`).

## What this build adds

All of it is in the Mac and Linux GUI. The daemon work below is small.

- **Chat layout.** A third layout next to classic and stage: a circle list
  and a message thread per circle, with replies, tap-to-answer choices, read
  ticks, search, photos and files, and a tasks card. It talks to an
  **Invoke** server (see the requirements below).
- **Stage layout.** One circle's lead up front and live, a strip of circles
  with their states, needs-you and done chips (with Snooze 1h), the crew as
  cards, and a health footer.
- **Wall.** A tile per circle and a Today feed, reached with the
  ctrl+shift+space peek. In this build the layout switch cycles classic,
  stage and chat, and `seance ctl layout wall` opens Chat.
- **Spaces** as tabs, nine color schemes (including Fog, Parlor and
  Herbarium), a pixel creature per circle lead, an AFK chip, auto-sleep for
  idle Claude panes, docked file panes, and opt-in sounds. The full list is
  in [`CHANGELOG.md`](CHANGELOG.md) under Unreleased and 0.27.0.

## What you need

| Part | Needs |
|------|-------|
| The Mac app (GUI) | A Mac. The download is Apple Silicon (arm64) only. |
| Classic, Stage, Wall and terminals | A **daemon**. The Mac app's launch picker offers "connect to remote host" (an ssh destination) or "run locally". The docs describe the Mac as a thin client, and the daemon is meant to run on a Linux machine; I have not tested the Mac "run locally" option. |
| A Linux machine | Where the daemon (and your agents' terminals) normally live. It needs this same build of `seance` on the `PATH` that a non-interactive `ssh` sees, and **passwordless ssh** from the Mac (keys or agent). The daemon refuses a GUI whose version does not match exactly, so build both sides from this repository. |
| Tailscale | **Not required by the app.** Any ssh destination works. Tailscale shows up in two places: your Invoke server is probably on your tailnet, and `seance web` leaves remote access to Tailscale (`docs/WEB.md`). |
| Chat layout | An **Invoke** server, a separate service that is not in this repo. Point the app at it with `SEANCE_INVOKE_URL` or the file `~/.config/seance/invoke-url` (see below). There is no built-in default. It expects the server to identify you itself (Tailscale Serve), so there is no token. With no URL set, Chat shows a message saying how to set one; use classic or stage instead. |
| Stage extras | Optional `terminal.conf` keys `seance-circle-map` and `seance-health-file`, files on the daemon's machine. Leave them out and those parts stay hidden. |

## Install

**Fastest, no building: the Mac app.**

1. Download `Seance-<version>-macos-arm64.zip` from this repository's
   Releases page and unzip it.
2. Drag `Seance.app` to `/Applications`.
3. It is **not Apple-signed**, so macOS blocks the first open. Do one of:
   right-click the app, choose **Open**, then **Open** again; or System
   Settings, Privacy & Security, **Open Anyway**; or run
   `xattr -dr com.apple.quarantine /Applications/Seance.app`.
4. Set up a daemon (next section), then pick "connect to remote host" in
   the launch picker.

**Build the Mac app yourself.** You need a recent Rust toolchain and the
Xcode command line tools.

```bash
git clone <this repository's URL> seance-build
cd seance-build
./scripts/bootstrap-deps.sh     # pinned gpui checkout
./scripts/bundle-macos.sh --user   # builds (about 10 to 15 min), installs ~/Applications/Seance.app
```

`bundle-macos.sh` also links `~/.local/bin/seance` to the app. See
[docs/REMOTE.md](docs/REMOTE.md).

**Set up the daemon on a Linux machine.**

```bash
git clone <this repository's URL> seance-build
cd seance-build
./scripts/bootstrap-deps.sh
cargo build --release           # Vulkan-capable drivers and a recent Rust
install -m755 target/release/seance ~/.local/bin/seance   # must be on the ssh PATH
```

Then open the Mac app, choose "connect to remote host" and type the ssh
destination. The app starts the daemon over ssh if it is not running.

**Using Chat.** The Chat view needs an Invoke server URL. Set either the
`SEANCE_INVOKE_URL` environment variable, or the file
`~/.config/seance/invoke-url` (one line, the `https://` URL). The variable
wins when both are set. A Dock launch does not see your shell's environment,
so the file is the easy way for the app. With neither set, Chat shows a
message saying how to set it.

---

# seance

**A shared space where humans and agents work together, live.**

Seance is a candlelit multi-pane terminal for Linux. Every pane is on the
human’s screen. Agents (Claude, Codex, Grok, any CLI) and shells sit beside
you — not hidden in a background job. They can see each other, ask you
questions, propose commands for your approval, and leave notes on a scratchpad
you both flip into. Visibility is the point.

Native app on [GPUI](https://www.gpui.rs/). Sessions live in a long-lived
daemon; the window is disposable.

**License:** MIT · **Platform:** Linux (Wayland / X11) + macOS thin client · **Status:** 0.27.0

Release notes: [`CHANGELOG.md`](CHANGELOG.md).

## Why it exists

Most agent tooling optimizes for *the agent alone*. Seance optimizes for
**engagement in a shared space**:

| human | agent |
|-------|--------|
| watches every pane live | runs in a real terminal on that screen |
| flips notes, steers, takes over a shell | drives siblings via `seance ctl` |
| answers `ask` toasts; Enter/Esc on ghost commands | prefers `propose` over silent risk |
| triages by status badges + stage strip | reports `planning` / `working` / `needs-human` |
| inspects pad drawer without flipping | opens file panes so edits appear live |

Attribution is first-class: actions are logged as `human` / `agent:<pane>` /
`cli`. The timeline answers “what happened while I was looking elsewhere?”

Any command is a pane. Default summon is a **shell** (so you can always take
the keyboard). Point `--command` at whatever agent CLI you use.

## Features

- **Live multi-pane terminals** — real PTYs, selection, scrollback; weighted tile grid with drag sashes (n≥2)
- **Spaces** — the rail lists spaces, then the circles in the one you're looking at. Drag a circle onto a space to move it; drag spaces to reorder them. A circle stays where you put it; a new one lands in the space on screen. One space is the default and catches everything else. Filter chips (pins, AFK, slack…) narrow the list across spaces without moving anything. `ctl space` / `ctl move` do the same from scripts.
- **Notes on the back of every pane** — shared markdown (`$SEANCE_SCRATCHPAD`)
- **Pad drawer** — stage chip / ▤ shows task inject body + pad tail (live-refreshes)
- **Stage strip** — urgency-sorted roster chips (click focus+pad, double-click zoom)
- **File panes** — live markdown/text + history/diff when co-editing
  - They open docked, in a panel on the right of their circle with one tab per file, so terminals keep the grid. The dock button in a file pane's title strip moves it into the grid and back.
- **Control plane** — `seance ctl` so any pane (or external script) can spawn, send, wait, harvest
- **Orchestrator A+** — `--agent` profiles, evidence-bound `wait --status done`, `send --file`, task envelopes, `harvest`, event-driven wait, boot-clear
- **Human-in-the-loop** — `ask`, `propose`, seize/release/drive
- **Phone a pane (CLI)** — `ctl phone` opens a telegram topic and seeds a **stage card** (workspace, roster, ctl how-to). No participant claim — you drive panes with normal `seance ctl` on this host. Optional needs-human one-liners post to the topic when linked.
- **Browser thin client** — `seance web` serves a wasm/WebGL2 client with native-parity chrome over a token-authed websocket
- **Session replay** — always-on 48h DVR; prompt-chapter player, trim/publish editor, shareable static bundles
- **Daemon architecture** — upgrade binary without killing the circle (concurrent-upgrade gate)
- **Event bus** — sequenced, attributable events + `seance ctl watch`
- **Capabilities** — `policy open|propose_required|locked` + per-principal grants

## Quick start

```bash
./scripts/bootstrap-deps.sh    # pinned gpui checkout — see docs/PLAYBOOK.md
cargo build --release          # first build can take ~10 min
./target/release/seance

ln -sf "$(pwd)/target/release/seance" ~/.local/bin/seance   # optional
```

Requirements: recent Rust, Vulkan-capable drivers, a monospace font
(default *JetBrainsMono Nerd Font* — change in `src/term_font.rs`).

On **macOS** (thin client against a remote daemon — see
[docs/REMOTE.md](docs/REMOTE.md)), `./scripts/bundle-macos.sh` is the build
command: it wraps the binary in `/Applications/Seance.app` so it launches from
Finder, Spotlight and the Dock. `--user` installs to `~/Applications`,
`--no-build` bundles what's already built. The bundle holds a *copy* of the
binary, so re-run the script rather than a bare `cargo build --release`.
It also installs `~/.local/bin/seance`, pointing into the app. The CLI follows
the same saved remote host as the GUI, even when the GUI is closed.
After installing a new build, **Seance → Restart Seance** in the menu bar
quits and reopens the app. Panes live on the daemon, so nothing is lost.
Add `--with-skills` to install the `seance-control` skill for local ChatGPT
Work, Codex, and Claude. See [remote CLI setup](docs/REMOTE.md#cli-files).

```bash
seance --help                    # command reference (same as seance ctl --help)
seance ctl skill                 # agent-facing protocol (⚡ arm / paste)
seance ctl doctor
seance ctl roster
seance ctl new --name w --agent claude --wait-ready
seance ctl send w --file /tmp/task.md        # busy: queued; idle: in, old task parked
seance ctl send w --file /tmp/task.md --now  # cancel w's open task, replace it
seance ctl wait w --status done --timeout 600 --cat
seance ctl harvest w1 w2 w3 --timeout 900
seance ctl phone w               # telegram topic + stage card (no claim)
```

`seance -h` and `seance help` also show the command reference.

Multi-agent live test: `./scripts/agent-collab-test.sh`  
Thorough smoke: `./scripts/e2e-thorough.sh`  
Upgrade load test: `./scripts/upgrade-load-test.sh` (upgrades live daemon)

## Claude Code hooks (status + toasts without arming)

Status chips, stage-strip urgency, and desktop toasts are **agent-reported** —
seance never guesses what an agent is doing. An un-armed agent reports
nothing, so a Claude launched by hand in a pane is invisible to attention
routing. For Claude Code you can make reporting automatic: every pane exports
`$SEANCE_SESSION`, and Claude Code hooks inherit it, so a tiny hook script
covers the whole turn lifecycle — no arming, nothing for the model to
remember, and a safe no-op outside seance.

```bash
#!/usr/bin/env bash
# ~/.local/bin/seance-claude-hook
[ -n "$SEANCE_SESSION" ] || exit 0
command -v seance >/dev/null 2>&1 || exit 0
case "$1" in
  working) seance ctl status-set working >/dev/null 2>&1 ;;
  stop)
    # Turn ended — your move. Toast only for panes you are NOT looking at;
    # the one on screen just flips its chip.
    f=$(seance ctl human --json 2>/dev/null | jq -r '.data.focused_pane // empty')
    if [ "$f" = "$SEANCE_SESSION" ]; then
      seance ctl status-set idle "turn ended" >/dev/null 2>&1
    else
      seance ctl status-set needs-human "turn ended — your move" >/dev/null 2>&1
    fi ;;
  needs-human)
    # Notification event (permission prompt / waiting on input); JSON on stdin.
    m=$(jq -r '.message // empty' 2>/dev/null)
    seance ctl status-set needs-human "${m:-claude needs attention}" >/dev/null 2>&1 ;;
esac
exit 0
```

```jsonc
// ~/.claude/settings.json
{ "hooks": {
  "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "seance-claude-hook working" }] }],
  "PostToolUse":      [{ "hooks": [{ "type": "command", "command": "seance-claude-hook working" }] }],
  "Stop":             [{ "hooks": [{ "type": "command", "command": "seance-claude-hook stop" }] }],
  "Notification":     [{ "matcher": "", "hooks": [{ "type": "command", "command": "seance-claude-hook needs-human" }] }]
} }
```

`working` on prompt submit and tool use (the latter clears a `needs-human`
left by an approved permission prompt), `needs-human` — a desktop toast —
when a turn ends off-screen or Claude blocks on a permission, `idle` when the
turn ends in the pane you are watching. Arming (`⚡` / `ctl skill`) is still
worth it for pads, `finish`, `ask`, and `propose` — hooks just make the
attention routing free.

## Keybinds

| key | action |
|-----|--------|
| ctrl+shift+n | new pane (shell by default) |
| ctrl+shift+w | banish (kill) active pane |
| ctrl+shift+s | flip notes ↔ face |
| ctrl+shift+p | pin / unpin selected circle (shows under the pins chip) |
| ctrl+shift+home | jump to the first circle in the rail's list |
| ctrl+shift+1 … 9 | jump to the nth circle in the rail's list |
| ctrl+shift+o | pop pane to its own window |
| ctrl+shift+k | precanned prompt palette |
| ctrl+shift+j | fuzzy jump (workspace) |
| ctrl+shift+z | focus-zoom active pane |
| ctrl+shift+space | peek at the Wall (a tile per circle); again or esc goes back |
| ctrl+shift+t | next layout: classic → stage → chat |
| ctrl+shift+i | next pane waiting on you, any circle or space (needs you, then done); again for the one after |
| ctrl+shift+r | rename selected workspace |
| ctrl+shift+f | last failed shell command |
| ctrl+pageup / pagedown | cycle circles in the rail's list |
| ctrl+alt+pageup / pagedown (macOS: cmd+option+↑↓) | show the previous / next space |
| ctrl+shift+pageup / pagedown | cycle panes in this workspace |
| ctrl+shift+v | paste |
| ctrl+click (macOS: cmd+click) / middle-click | open OSC-8 / URL in the default browser |
| double-click a file path | show it in Finder (Linux: open its folder); images, videos, PDFs and HTML open in their app |
| mouse back / forward | walk the circles you've been in |
| stage chip click | focus + pad drawer |
| stage chip double-click | zoom |
| Stage × (crew card, on-stage header) | close that pane: click once to arm, again to close (the last pane closes its circle) |
| ⚡ | arm agent (`ctl skill` orientation) |
| ▤ | pad drawer |
| sidebar right-edge drag | resize sidebar (width persists) |
| click a space | show it and select its first circle |
| drag a circle onto a space | move it there (sticky) |
| drag a Stage pill onto another | reorder circles; the rail, Stage and Wall all follow, and rows never move on their own |
| drag a space onto another | reorder spaces (onto the divider = to the end) |
| space right-click | rename · make default · banish all · delete |
| space tab right-click (Stage, Wall) | rename · make default · delete |
| pane right-click (rail row, Stage crew card) | mark unread / mark read (crew card: rename too) |
| double-click a pane title (tile, Stage) | rename that pane |
| chip right-click | pins: unpin all · a host mode: turn it off for every circle in it |
| sash drag | resize 2-pane ratio or multi-pane weights |
| macOS: cmd+backspace / cmd+←→ / option+←→ | kill to line start / line start, end / word back, forward (as in Ghostty) |
| option+backspace (alt on Linux) | delete previous word |

## Layouts

Each window has a layout, picked from the **classic · stage · chat** toggle
(under the logo in the rail, in the top bar elsewhere), with ctrl+shift+t
(cmd+shift+t on macOS), or with `seance ctl layout NAME`.

- **classic**: the rail and pane grid, exactly as before. The default.
- **stage**: one circle's lead up front. See below.
- **wall**: a tile per circle and a Today feed. See below. Leaving it with
  esc or ctrl+shift+space returns to the layout you opened it from; Focus
  goes to stage.

Windows choose independently, so the laptop can sit on stage while the desk
stays classic. The last pick is saved on the GUI's own machine in
`~/.config/seance/view.json` (`$XDG_CONFIG_HOME/seance/` when set) and
restored on launch; the daemon does not store it. A ctrl+shift+space peek at
the wall is not saved, and a blank second window always opens on classic. `seance ctl layout` lists
the layouts and each attached window's current one; `seance ctl layout NAME`
switches every attached window at once.

### Stage

Stage trades the rail for a strip across the top and puts one circle's lead
in the middle of the window, live and typeable. From top to bottom:

- **Space tabs.** When two or more spaces have circles, a thin row of tabs
  runs across the top, led by the seance mark: one tab per space, with the
  same mark its rail row has (the needs-you dot, the working spinner, the
  done dot; nothing when quiet). The space on show is bright and
  underlined. Click a tab to show that space; right-click to rename it,
  make it the default or delete it. Empty spaces get no tab. The row
  scrolls sideways when the tabs don't fit; ctrl+alt+pageup / pagedown
  (cmd+option+↑↓ on macOS) steps through the spaces from the keyboard.
- **The strip.** ✦ opens the same menu as the rail's. Then the circles of the
  current space as pills, in rail order, each with its state: ✓ done,
  ◐ working, a violet dot when it needs you, ✕ exited, a faint dot when idle.
  Click one to switch. Pills that don't fit fold into "+N more", which opens
  the jump palette. **+ circle** after them makes a new circle in this
  space and opens its name; Enter names it and opens its first pane (a
  shell, or `seance-new-circle-command`), Esc leaves it empty. Then the
  layout toggle and a jump box (ctrl+shift+j, the
  same jump palette).
- **Needs you.** Only when something waits (or is snoozed): one chip per
  pane, in every circle, that is needs-human, blocked or risky or has asked
  a question.
  **Open** switches to that circle and puts the pane on stage. After them, a
  green ✓ chip per circle with a finished pane you haven't seen, and one per
  finished pane in the circle you're on. Click a ✓ chip to peek: the pane's
  latest `ctl finish` note, when that report was filed (with the day, if it
  wasn't today), and under **Last reply** the first line of what the pane
  said last, when it says something the report doesn't. **Open** goes to
  the pane; **Seen** clears the mark without opening it. Last come
  **Unread** chips, one per pane you marked unread, until you open the
  pane. ctrl+shift+i walks the needs-you and done items in any layout (on
  the Wall it selects the circle's tile). **Snooze 1h** on a needs-you or
  done chip (or the peek card) quiets that pane for an hour: no chip, no
  banner or ding, and ctrl+shift+i skips it. Its row and
  status stay. Opening the pane ends it early; when the hour is up the chip
  comes back if it's still waiting. A faint "N snoozed" at the end of the
  bar brings them all back (when only snoozed ones are left, the bar shows
  just that). Snoozes are kept in `~/.config/seance/view.json`, so they
  survive a restart; a blank second window keeps its own for the session.
- **The lead.** The circle's lead pane (`seance ctl lead`, else its first
  terminal) is the real terminal: keys, esc and every chord work as in any
  focused pane. Its header shows the name, the machine (Mac or the daemon's host, from a
  name ending `· Mac` or a `claude-mac` command), its state and how long.
  Under it, a **Doing / Next / Waiting on** line from the top entry of the
  circle's `LEAD-LOG.md` (see `seance-circle-map` below); circles without a
  log just skip the line. When a crew pane finishes while you're looking
  elsewhere, a **Crew** line names it, says when, and quotes the first line
  of its reply, until you open that pane. **mark unread** in the header
  marks the pane on stage to come back to (**mark read** clears it).
- **Crew.** The circle's other terminals as cards: name, machine, state and
  the pane's last line of output. Shells with nothing running and sleeping
  panes are dashed. Click a card to put that pane on stage; "back to lead"
  returns. Right-click a card to mark it unread. "+ worker" opens a new pane in the circle. Below the crew, the
  circle's **files** as tabs (the dock panel stays out of the way on stage).
- **Health footer.** The daemon host's and the Mac's state, the leads line, Claude plan use and
  the report time, from the file named by `seance-health-file`.

On windows narrower than about 1000px the crew folds behind a "crew · N"
button in the lead's header, so the terminal keeps its width.

Two optional `terminal.conf` keys feed Stage. Both name files **on the
daemon's machine** and are read through the daemon, so a thin client sees
the daemon's copies and a leading `~` means the daemon's home:

```
seance-circle-map = ~/projects/circles.tsv          # circle-slug<TAB>folder
seance-health-file = ~/.local/state/seance-health/latest.md
```

The circle map is a TSV of `circle-slug<TAB>folder` (`#` lines are comments);
a relative folder sits beside the map, and Stage reads
`<folder>/LEAD-LOG.md`, every 30s at most and on each circle switch. The
lead log's newest `## ` entry supplies the line, from bullets such as
`- **Doing:** ...`, `- **Next:** ...` and `- **Waiting on:** ...`. The health
file is re-read every minute; its first line is
`# Seance health <date> <time>: <status>`; a bullet `- <host> memory: 2.5G of
7.4G free (34%)` names the daemon's machine in the footer ("Host" if there
is none). Leave a key out and its part of
Stage stays hidden.

`seance-new-circle-command` is what the first pane of a circle made with
**+ circle** runs, on the daemon's machine like any pane (unset: a plain
shell):

```
seance-new-circle-command = claude-mac
```

### Wall

The Wall replaces the rail and grid with a tile for every circle of the
current space (the rail's list, in rail order) and a Today feed down the
right side.

- **Header.** The space tabs, as on Stage, sit above it, led by **All**:
  every circle from every space in one grid, in space order, each small
  tile with its space's name. Focus from All opens Stage in the circle's
  own space; a space tab leaves All. The pick is kept on this machine
  (`wall_all` in `~/.config/seance/view.json`). ✦ opens the rail's
  menu. Then a **Wall · Focus** switch (Focus opens the selected circle on stage),
  a summary such as "9 circles awake · 1 needs you · 1 down", the layout
  toggle and a jump box for the jump palette (ctrl+shift+j).
- **Tiles.** The selected circle is a big tile, two columns by two rows,
  with its lead live (read only) and the lead's name, machine, crew count
  and state in its header, plus a **Focus** button. Every other circle
  shows its state, the lead's current task (its terminal title), the
  **Doing** line from its `LEAD-LOG.md`, the first lines of the lead's
  last `ctl finish` result ("3h ago:" once it is over an hour old, gone
  after a day) and a "Next:" or "Waiting on:" line. Only a circle with
  none of those shows the lead's last line of screen output instead. A circle that needs you
  has a violet ring and an **Open** button that opens stage with the pane
  that asked; a circle whose lead has exited is red with **Open**; a lead
  waiting on a sleeping Mac (`claude-mac`'s message) is dashed and dimmed.
  Circles put to sleep fold into one "+N asleep" tile; click it to show
  them. The grid is four columns, three or two on narrower windows, and
  scrolls when there are more circles than fit.
- **Today.** The day's events from the daemon's event log, newest first, up
  to 50: needs-you raised, done (with how long it worked), panes exited,
  lead changes, circles put to sleep or woken, the health check report and,
  while this window watched, a lead's Mac going to sleep or coming back. The
  health footer from Stage sits under it. Below about 1100px the feed folds
  behind a "Today" button.

Keys on the Wall: arrows move the selection between tiles, Enter (or a
double click, or Focus) opens the selected circle on stage, with the pane
that is waiting on you up front when there is one, esc goes back.
A single click selects. Typing reaches no pane while the Wall is up, and
the prompts palette (ctrl+shift+k) does nothing there; the other app chords
still work. Every circle's grid streams to the Wall while it is
showing and stops when it is not.

## Terminal settings (Ghostty syntax)

`~/.config/seance/terminal.conf` sets the terminal look and alerts, using
Ghostty's config keys. Each machine reads its own: the daemon resolves the
palette, a GUI uses the font, default fg/bg (pane padding), sound and command
rule, so give a thin client and its daemon the same theme. Include your
Ghostty config to reuse it:

```
config-file = ?~/.config/ghostty/config   # font-size, bell, notify rules
theme = Monokai Pro                       # any Ghostty theme name or path
```

Color schemes sit on top of this: `seance ctl theme` (or the ✦ menu at the
top of the rail) switches chrome and terminal colors together for every
window, and the daemon remembers the pick. Candlelit, the default, keeps the
palette from this file; `seance-theme = ID` here only picks the scheme a
daemon starts on before anyone has chosen one. See docs/THEME.md.

Read keys: `seance-theme`, `seance-circle-map`, `seance-health-file` (Stage,
see above), `theme`, `palette`, `background`, `foreground`, `cursor-color`,
`font-family`, `font-size`, `bell-features` (`audio`), `bell-audio-path`,
`bell-audio-volume`,
`notify-on-command-finish` (`never` / `unfocused` / `always`),
`notify-on-command-finish-after`, `seance-needs-sound` (a macOS sound name,
`default` = the system's, for needs-you and agent-ask banners),
`seance-done-sound` and `seance-done-sound-volume` (played when a pane
finishes while you're looking elsewhere; leads only),
`seance-auto-sleep-idle` and `seance-auto-sleep-skip` (see below),
`seance-new-circle-command` (Stage, see above),
`seance-origin-gutter` (`false` hides the 2px stripe at a terminal's left
edge that tints by who last typed).
A pane's BEL plays the bell sound; OSC 9 /
OSC 777 and long shell commands post a desktop notification when you
aren't looking at that pane (a command-finish notification also plays the bell sound); OSC 52
copies reach the clipboard. Restart the daemon (`seance upgrade`) and GUI
after editing.

**Auto-sleep.** A Claude pane nobody has used for a while is put to sleep:
its process stops and frees its memory, and the pane keeps its name, place
and last screen. Only panes that can come back exactly are slept (a Claude
pane whose conversation is on disk, including Claude started from a shell
or a wrapper), and never one that is busy, waiting on you or focused.
Typing into a sleeping pane, `seance ctl send` to it, or **awaken** wakes it
into the same conversation.

```
seance-auto-sleep-idle = 6h            # default 12h; off turns it off
seance-auto-sleep-skip = alpha-circle, main   # circles never auto-slept
```

### Lead sprites

Each circle's lead pane shows a small pixel creature, in its title strip
and beside **LEAD** on Stage. It is generated from the circle's slug, so
it stays the same everywhere. To re-roll one, give it another seed:

```
seance-lead-sprite.alpha-circle = frog
```

## Architecture (short)

| process | role |
|---------|------|
| `seance daemon` | owns PTYs, grids, state; Unix socket |
| `seance` (GUI) | shared space UI; reconnects safely |
| `seance ctl …` | JSON-lines client for agents, shells, scripts |
| `seance web` | ws↔socket bridge + static client; browsers attach as GUI clients |

**Do not** `pkill -x seance` to reload — that kills every session. Prefer
`cargo build --release && seance upgrade`, or `seance restart-gui` for UI-only.

| path | |
|------|--|
| state | `~/.local/share/seance/state.json` |
| scratchpads | `~/.local/share/seance/scratch/<slug>.md` |
| layout weights | `~/.local/share/seance/layout.json` |
| rail arrangement | `~/.local/share/seance/subscriptions.json` (daemon-owned; every window shares it) |
| events | `~/.local/share/seance/events.jsonl` |
| socket | `$XDG_RUNTIME_DIR/seance.sock` |

Injected into every pane: `SEANCE_SESSION`, `SEANCE_PANE_ID`,
`SEANCE_WORKSPACE`, `SEANCE_WORKSPACE_ID`, `SEANCE_WORKSPACE_NAME`,
`SEANCE_SCRATCHPAD`, `SEANCE_SOCKET`. The two `_ID` vars are lifetime UUIDv4s:
saved with the pane/circle, preserved through rename, sleep/wake and restore,
never reused after deletion. Use them for durable integrations; slugs can be
reused. `ctl list --all --json` and `ctl whoami --json` expose `pane_id` and
`workspace_id`; pane targets accept `id:<pane_id>`. Legacy state gains IDs
on load and saves them immediately. Existing processes retained by a live
upgrade get the env vars on their next wake/restore; ctl has the IDs immediately.
Workspace scoping is automatic inside a pane — agents only see their circle
unless you pass `--all`. See [CONTROL.md](docs/CONTROL.md#session-environment).

## Docs

| doc | |
|-----|--|
| [docs/CONTROL.md](docs/CONTROL.md) | control plane + how agents engage the human |
| [docs/WEB.md](docs/WEB.md) | browser thin client (`seance web`): wasm renderer, ws bridge, token auth |
| [docs/REPLAY.md](docs/REPLAY.md) | session replay: 48h DVR, prompt-chapter player, trim/publish editor |
| [docs/DAEMON.md](docs/DAEMON.md) | daemon / GUI split, upgrade path |
| [docs/ORCHESTRATION.md](docs/ORCHESTRATION.md) | multi-agent swarm playbook |
| [docs/SHELL-INTEGRATION.md](docs/SHELL-INTEGRATION.md) | structured command boundaries |
| [docs/REMOTE.md](docs/REMOTE.md) | thin client against a remote daemon (ssh forward) |
| [docs/PERF-TERMINAL.md](docs/PERF-TERMINAL.md) | multi-pane paint notes (native + web) |
| [docs/THEME.md](docs/THEME.md) | candlelit palette, `SeancePalette` |
| [CLAUDE.md](CLAUDE.md) | notes for coding agents working *on* this repo |

Canonical agent instructions: **`seance ctl skill`**.

## Develop

```bash
./scripts/bootstrap-deps.sh
cargo test
cargo build --release && seance upgrade
./scripts/e2e-thorough.sh
```

Pin discipline: `gpui-component` rev-pinned; zed patched to `deps/zed` at
`1a246efd…`. Bump only as a pair — PLAYBOOK.

## Not yet

- OSC-133 shell-agnostic markers (bash hooks + cmdlog work today; OSC-8 open shipped)
- GPU glyph atlas **in the native app** (CPU path is multi-pane smooth — explicit
  non-goal for now; the web client is WebGL2 already)
- worktree-backed agent rooms, best-of-N

## License

MIT — see [LICENSE](LICENSE).

Uses [zed’s alacritty fork](https://github.com/zed-industries/alacritty)
(Apache-2.0), [GPUI](https://www.gpui.rs/), and
[gpui-component](https://github.com/longbridge/gpui-component).
