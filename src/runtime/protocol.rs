//! Wire types for daemon ↔ GUI and connection hello.
//!
//! The client-facing wire (Hello, GuiRequest/GuiEvent, FsOp, pane/ask/status
//! info) lives in `seance-core` — shared verbatim with the web client. This
//! module re-exports it and keeps the native-only handoff types (they carry
//! CommandLog/AgencySnap, which are daemon-internal).

use serde::{Deserialize, Serialize};

use super::snapshot::GhostSnap;

pub use seance_core::protocol::*;

/// The hello line every same-version client sends (role = "ctl" or "gui").
pub fn hello_line(role: &str) -> String {
    seance_core::protocol::hello_line_with(role, env!("CARGO_PKG_VERSION"))
}

/// Handoff message (old daemon → new) — FDs travel out-of-band via SCM_RIGHTS.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffPane {
    #[serde(default)]
    pub pane_id: String,
    pub name: String,
    pub slug: String,
    pub workspace: String,
    pub cwd: String,
    pub command: String,
    pub tiled: bool,
    pub resume_on_restore: bool,
    /// See `PersistedPane::agent_session`. Alias: handoff from a pre-rename daemon.
    #[serde(default, alias = "claude_session")]
    pub agent_session: Option<String>,
    /// Slept panes hand off as slept — no PTY fd to transfer, nothing to do.
    #[serde(default)]
    pub asleep: bool,
    pub kind: String,
    pub file: Option<String>,
    pub child_pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    /// Master PTY fd index into the SCM_RIGHTS list (terminal panes only).
    pub fd_index: Option<usize>,
    pub title: Option<String>,
    pub text_snapshot: String,
    pub ghost: Option<GhostSnap>,
    /// Co-presence state (0.9.5+). Missing → default agency.
    #[serde(default)]
    pub agency: Option<crate::agency::AgencySnap>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffBundle {
    #[serde(default)]
    pub workspace_ids: std::collections::BTreeMap<String, String>,
    pub panes: Vec<HandoffPane>,
    pub selected_workspace: Option<String>,
    pub focused_pane: Option<String>,
    pub extra_workspaces: Vec<String>,
    pub workspace_order: Vec<String>,
    /// slug → display label, for circles whose label differs from their slug.
    #[serde(default)]
    pub workspace_names: Vec<(String, String)>,
    pub proposal_counter: u64,
    pub ask_counter: u64,
    /// Live badges (0.9.5+) — survive `seance upgrade`.
    #[serde(default)]
    pub statuses: Vec<StatusInfo>,
    /// Unanswered asks (0.9.5+).
    #[serde(default)]
    pub asks: Vec<AskInfo>,
    /// Per-pane pad revision counters.
    #[serde(default)]
    pub pad_revs: Vec<(String, u64)>,
    /// Inject baselines for evidence-bound wait.
    #[serde(default)]
    pub inject_baselines: Vec<InjectBaseline>,
    /// Open/recent tasks (0.9.6).
    #[serde(default)]
    pub tasks: Vec<TaskRecord>,
    #[serde(default)]
    pub task_counter: u64,
    /// pane slug → active task id
    #[serde(default)]
    pub active_tasks: Vec<(String, String)>,
    /// Shell command log (0.9.11 — survive upgrade).
    #[serde(default)]
    pub cmd_log: crate::cmdlog::CommandLog,
    /// workspace → last real pane output (unix ms). Daemon-owned activity
    /// clock; absent on older daemons → clocks start unknown, not zero.
    #[serde(default)]
    pub workspace_output: Vec<(String, u64)>,
    /// workspace → last human input (unix ms).
    #[serde(default)]
    pub workspace_touch_ms: Vec<(String, u64)>,
    /// pane slug → spawning pane slug (`ctl new` from inside a pane).
    #[serde(default)]
    pub pane_parents: Vec<(String, String)>,
    #[serde(default)]
    pub comms: CommsState,
}

/// Cross-session communication state (docs/COMMS.md): who leads each circle,
/// what circles used to be called, who talks to whom, and the messages
/// themselves. Daemon-owned and persisted (state.json + upgrade handoff).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CommsState {
    /// circle slug → pane slug set by `ctl lead` (else the first pane leads).
    #[serde(default)]
    pub leads: std::collections::BTreeMap<String, String>,
    /// circle slug → labels it had before, oldest first (rename aliases).
    #[serde(default)]
    pub former_labels: std::collections::BTreeMap<String, Vec<String>>,
    /// circle slug → panes (outside it) that have talked to it; they get a
    /// notice when it is renamed or its lead closes.
    #[serde(default)]
    pub contacts: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    /// pane slug → spawn time (ms); the earliest pane of a circle leads it.
    #[serde(default)]
    pub born: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub messages: Vec<MessageRecord>,
    #[serde(default)]
    pub counter: u64,
    /// circle slug → host circle modes on there (e.g. "afk"); see host.json
    /// `circle_modes`.
    #[serde(default)]
    pub modes: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    /// (circle, mode) → (on, until ms): a menu toggle of a host-polled mode
    /// (`state_cmd`) the host hasn't confirmed yet. Holds the badge until the
    /// poll agrees or it expires. Not persisted — a restart just re-polls.
    #[serde(skip)]
    pub mode_pending: std::collections::BTreeMap<(String, String), (bool, u64)>,
}

/// One message between sessions (`ctl ask --to` / `tell` / `reply`, and
/// daemon notices). Never a task: delivering it opens and cancels nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: String,
    /// ask | tell | reply | notice
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_circle: Option<String>,
    pub to_pane: String,
    /// Set when addressed to a circle (delivered to its lead; re-routed to the
    /// new lead if that pane closes first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_circle: Option<String>,
    pub text: String,
    pub created_ms: u64,
    /// pending | delivered | answered | failed
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    /// reply: the ask it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_ms: Option<u64>,
    /// Last time a `ctl ask`/`await` was blocked on this ask (ms). A reply
    /// while someone is waiting goes to their stdout, not into their pane.
    #[serde(default)]
    pub waiter_seen_ms: u64,
    /// notice: the circle it is about (newer notices replace older ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
