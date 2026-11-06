//! Sidebar context-menu actions (menu items dispatch gpui actions) and the
//! one-click "arm" prompt injected into a fresh agent pane.

use gpui::Action;
use serde::Deserialize;

/// Quit the app. Exists for the macOS menu bar: a bundled `.app` has no
/// terminal to ctrl-C and no window-close-quits convention, so without a
/// Quit item bound to ⌘Q there is *no* way to stop seance short of force
/// quit. Dispatched from the menu; `cx.on_app_quit` still tears the tunnel
/// down, so this only has to ask.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActQuit;

/// Quit and open again: a fresh GUI on the same daemon (panes live there,
/// so nothing is lost).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActRestart;

// Sidebar context-menu actions (menu items dispatch gpui actions).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActToggleTiled(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActOpenNotes(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActKillSession(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActMoveToWorkspace {
    pub slug: String,
    pub workspace: String,
}

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActMoveToNewWorkspace(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActTogglePopout(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActKillWorkspace(pub String);

/// Sleep a circle: every pane's process exits, the last frame stays readable,
/// and it wakes back onto the same conversations. Daemon-side and global.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActSleepWorkspace(pub String);

/// Wake a sleeping circle (the awaken bar, and the context menu).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActWakeWorkspace(pub String);

/// Pin a circle into the sidebar's top section.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActPinWorkspace(pub String);

/// The inverse: back down into the normal band.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActUnpinWorkspace(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActRenamePane(pub String);

/// Prompt injected by the one-click "arm" action — orients an agent in a
/// seance pane so it uses the control plane instead of flying blind. A thin
/// pointer to `seance ctl skill` on purpose: the contract lives in one place.
/// Panes that load the skill at session start (docs/CONTROL.md, "Agent
/// skill") never need arming.
pub(crate) const SEANCE_ARM_PROMPT: &str = "\
You are inside **seance** — a shared live workspace where humans and agents \
work in the open. Every pane is on my screen; visibility is the point.

Run `seance ctl whoami` (your pane and circle) and `seance ctl skill` (the \
engagement protocol: driving sibling panes, talking to other circles, status, \
scratchpads, file panes), and follow that protocol from now on. The skill ships \
with seance, so it is always current; re-run it after a context reset.

Confirm you're oriented and ready, then wait for the next instruction.";

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActRenameWorkspace(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActRenameSpace(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActDeleteSpace(pub String);

/// (pane slug, true to mark it unread / false to clear the mark).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActMarkUnread(pub String, pub bool);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActNewSpace(pub bool);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActSpaceDefault(pub String);

/// (space id, -1 up / +1 down).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActSpaceShift(pub String, pub i32);

/// Arm (first) / fire (second, within the arm window) banishing every
/// circle in a space.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActBanishSpace(pub String);

#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActUnpinAll(pub bool);

/// (circle, space id).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActMoveToSpace(pub String, pub String);

/// Turn a host mode OFF for every circle in it (right-click its rail chip):
/// "i'm back" to all AFK circles at once.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActCircleModeAllOff(pub String);

/// Turn a host circle mode on/off: (workspace, mode id, on). Rail context
/// menu; only offered for modes host.json defines (e.g. AFK).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActCircleMode(pub String, pub String, pub bool);

/// Open the web replay editor for a workspace (sidebar context menu).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActShareReplay(pub String);

/// Open the quicklaunch editor pre-filled for the named entry (context menu).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActQuickLaunchEdit(pub String);

/// Remove the named quicklaunch entry and persist (context menu).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = seance, no_json)]
pub struct ActQuickLaunchRemove(pub String);
