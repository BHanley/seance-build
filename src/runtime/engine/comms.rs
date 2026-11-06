//! Cross-session communication: sessions talking to each other by the
//! names the human uses (docs/COMMS.md).
//!
//! - **Circles are addressed by label** (or slug, or a label they used to
//!   have), and a circle is answered by its **lead**: the pane set with
//!   `ctl lead`, else its first pane.
//! - **Messages are not tasks.** `ask` / `tell` / `reply` / notices are
//!   [`MessageRecord`]s delivered through the send-queue pump (queue.rs): they
//!   never open or cancel a task, and a busy agent gets them queued behind its
//!   turn instead of interrupted.
//! - **Replies route by message id**, so no one types a return address — the
//!   2026-09-29 misroute (a circle slug handed back as a reply-to pane) cannot
//!   happen through `reply`.
//! - **Contacts**: a pane that talks to another circle is recorded as its
//!   contact, and gets a notice when that circle is renamed or its lead
//!   closes. Old labels keep resolving (with a warning) until another circle
//!   takes them.

use std::collections::BTreeSet;

use serde_json::json;

use super::helpers::now_ms;
use super::Engine;
use crate::events;
use crate::runtime::protocol::*;

/// Messages kept in state (oldest non-pending dropped first).
const MESSAGE_CAP: usize = 500;

/// Where a message goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub pane: String,
    /// Set when the address named a circle (delivered to its lead).
    pub circle: Option<String>,
    /// e.g. "was renamed to …" when an old label matched.
    pub note: Option<String>,
}

fn is_agent_command(command: &str) -> bool {
    let c = command.to_ascii_lowercase();
    ["claude", "codex", "grok"].iter().any(|a| c.contains(a))
}

impl Engine {
    /// Remember when a pane was born (the earliest pane leads its circle).
    pub(super) fn note_born(&mut self, slug: &str) {
        self.comms
            .born
            .entry(slug.to_string())
            .or_insert_with(now_ms);
    }

    /// The pane that answers for `circle`: an explicit `ctl lead` if that
    /// pane is still in the circle, else its earliest terminal pane (panes
    /// with no recorded birth predate the record and count as oldest, in
    /// pane-list order).
    pub(crate) fn effective_lead(&self, circle: &str) -> Option<String> {
        let members = || self.panes.iter().filter(|p| p.workspace == circle);
        if let Some(l) = self.comms.leads.get(circle) {
            if members().any(|p| &p.slug == l) {
                return Some(l.clone());
            }
        }
        let pick = |terminal_only: bool| {
            members()
                .enumerate()
                .filter(|(_, p)| !terminal_only || p.kind != "file")
                .min_by_key(|(i, p)| (self.comms.born.get(&p.slug).copied().unwrap_or(0), *i))
                .map(|(_, p)| p.slug.clone())
        };
        pick(true).or_else(|| pick(false))
    }

    /// A circle by slug, current label, or a label it used to have. The
    /// second value says which old label matched.
    pub(crate) fn resolve_circle(&self, key: &str) -> Option<(String, Option<String>)> {
        if let Some(slug) = self.resolve_workspace(key) {
            return Some((slug, None));
        }
        let known = self.known_workspace_slugs();
        let mut hits = known.iter().filter(|s| {
            self.comms
                .former_labels
                .get(*s)
                .is_some_and(|l| l.iter().any(|x| x == key))
        });
        let first = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        Some((first.clone(), Some(key.to_string())))
    }

    /// Resolve a message address. `@name` is a circle; `pane:name` is a
    /// pane; a bare name may be either, and is refused when it is both (in
    /// different places) — `claude-27` was a circle slug AND another
    /// circle's pane, which is how a reply went to the wrong session.
    pub(crate) fn resolve_target(&self, key: &str) -> Result<Target, String> {
        let key = key.trim();
        if key.starts_with("id:") {
            return Ok(Target {
                pane: self.pane_slug_from_id(key)?,
                circle: None,
                note: None,
            });
        }
        let circle_target = |slug: String, renamed: Option<String>| -> Result<Target, String> {
            let label = self.workspace_label(&slug);
            let lead = self
                .effective_lead(&slug)
                .ok_or_else(|| format!("circle '{label}' has no panes"))?;
            Ok(Target {
                pane: lead,
                note: renamed.map(|old| format!("'{old}' was renamed to '{label}'")),
                circle: Some(slug),
            })
        };
        if let Some(name) = key.strip_prefix('@') {
            let (slug, renamed) = self
                .resolve_circle(name)
                .ok_or_else(|| self.no_such(name))?;
            return circle_target(slug, renamed);
        }
        if let Some(name) = key.strip_prefix("pane:") {
            let p = self
                .panes
                .iter()
                .find(|p| p.slug == name)
                .ok_or_else(|| format!("no pane '{name}'"))?;
            return Ok(Target {
                pane: p.slug.clone(),
                circle: None,
                note: None,
            });
        }
        let pane = self.panes.iter().find(|p| p.slug == key);
        let circle = self.resolve_circle(key);
        match (pane, circle) {
            (Some(p), Some((slug, renamed))) => {
                let lead = self.effective_lead(&slug);
                if lead.as_deref() == Some(p.slug.as_str()) {
                    return circle_target(slug, renamed);
                }
                Err(format!(
                    "'{key}' is ambiguous: circle '{}' (lead {}) and pane {key} in circle '{}'. \
                     Say @{key} for the circle or pane:{key} for the pane",
                    self.workspace_label(&slug),
                    lead.as_deref().unwrap_or("-"),
                    self.workspace_label(&p.workspace)
                ))
            }
            (None, Some((slug, renamed))) => circle_target(slug, renamed),
            (Some(p), None) => Ok(Target {
                pane: p.slug.clone(),
                circle: None,
                note: None,
            }),
            (None, None) => Err(self.no_such(key)),
        }
    }

    fn no_such(&self, key: &str) -> String {
        let circles: Vec<String> = self
            .known_workspace_slugs()
            .iter()
            .map(|s| self.workspace_label(s))
            .collect();
        format!(
            "no circle or pane '{key}' (circles: {})",
            circles.join(", ")
        )
    }

    /// `from` (a pane) talked to `circle`: remember it, unless it's its own.
    pub(super) fn record_contact(&mut self, from: Option<&str>, circle: &str) {
        let Some(from) = from else { return };
        let Some(own) = self
            .panes
            .iter()
            .find(|p| p.slug == from)
            .map(|p| &p.workspace)
        else {
            return;
        };
        if own != circle {
            self.comms
                .contacts
                .entry(circle.to_string())
                .or_default()
                .insert(from.to_string());
        }
    }

    pub(super) fn circle_of(&self, pane: &str) -> Option<String> {
        self.panes
            .iter()
            .find(|p| p.slug == pane)
            .map(|p| p.workspace.clone())
    }

    /// Record a message for delivery; returns its id.
    pub(crate) fn post_message(&mut self, mut rec: MessageRecord) -> String {
        self.comms.counter = self.comms.counter.saturating_add(1);
        rec.id = format!("m-{}", self.comms.counter);
        rec.created_ms = now_ms();
        if rec.status.is_empty() {
            rec.status = "pending".into();
        }
        // A newer notice about the same circle replaces an undelivered one.
        if let Some(about) = &rec.about {
            self.comms.messages.retain(|m| {
                !(m.kind == "notice"
                    && m.status == "pending"
                    && m.to_pane == rec.to_pane
                    && m.about.as_ref() == Some(about))
            });
        }
        let id = rec.id.clone();
        events::log(
            &rec.from_pane
                .as_ref()
                .map(|p| format!("agent:{p}"))
                .unwrap_or_else(|| "daemon".into()),
            rec.to_circle.as_deref(),
            Some(&rec.to_pane),
            &format!("msg_{}", rec.kind),
            format!("{id}: {}", rec.text.chars().take(80).collect::<String>()),
        );
        self.comms.messages.push(rec);
        self.prune_messages();
        id
    }

    fn prune_messages(&mut self) {
        while self.comms.messages.len() > MESSAGE_CAP {
            match self
                .comms
                .messages
                .iter()
                .position(|m| m.status != "pending")
            {
                Some(i) => {
                    self.comms.messages.remove(i);
                }
                None => break,
            }
        }
    }

    pub(crate) fn message(&self, id: &str) -> Option<&MessageRecord> {
        self.comms.messages.iter().find(|m| m.id == id)
    }

    pub(super) fn message_mut(&mut self, id: &str) -> Option<&mut MessageRecord> {
        self.comms.messages.iter_mut().find(|m| m.id == id)
    }

    /// Oldest pending message for `pane`.
    pub(super) fn pending_message_for(&self, pane: &str) -> Option<String> {
        self.comms
            .messages
            .iter()
            .filter(|m| m.status == "pending" && m.to_pane == pane)
            .min_by_key(|m| m.created_ms)
            .map(|m| m.id.clone())
    }

    pub(super) fn panes_with_pending_messages(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .comms
            .messages
            .iter()
            .filter(|m| m.status == "pending")
            .map(|m| m.to_pane.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// A human-readable label for a pane: "circle label (pane)".
    fn who(&self, pane: Option<&str>, circle: Option<&str>) -> String {
        match (pane, circle) {
            (Some(p), _) => {
                let c = self
                    .circle_of(p)
                    .map(|c| self.workspace_label(&c))
                    .unwrap_or_else(|| "?".into());
                format!("{c} (pane {p})")
            }
            (None, Some(c)) => self.workspace_label(c),
            (None, None) => "the human / a script".into(),
        }
    }

    /// The text pasted into the recipient's TUI.
    pub(crate) fn message_paste_text(&self, m: &MessageRecord) -> String {
        let from = self.who(m.from_pane.as_deref(), m.from_circle.as_deref());
        match m.kind.as_str() {
            "ask" => format!(
                "📨 seance {} — question from {from}:\n\n{}\n\n\
                 Answer it with `seance ctl reply {} <<'EOF'` … `EOF` (it routes back \
                 automatically; do not use send). Then carry on with your own work.",
                m.id, m.text, m.id
            ),
            "reply" => format!(
                "📨 seance {} — reply from {from} to your question {}:\n\n{}",
                m.id,
                m.reply_to.as_deref().unwrap_or("?"),
                m.text
            ),
            "notice" => format!("📇 seance notice: {}", m.text),
            // A circle-mode prompt (AFK on/off) is the human's own words: as is.
            "prompt" => m.text.clone(),
            _ => format!(
                "📨 seance {} — note from {from} (FYI, no reply needed):\n\n{}",
                m.id, m.text
            ),
        }
    }

    /// `reply ID`: store the answer, and deliver it to the asker's pane
    /// unless a `ctl ask`/`await` is blocked on it right now.
    pub(crate) fn reply_to_message(
        &mut self,
        id: &str,
        text: &str,
        from: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let ask = self
            .message(id)
            .cloned()
            .ok_or_else(|| format!("no message '{id}'"))?;
        if ask.kind != "ask" {
            return Err(format!(
                "{id} is a {}, not a question — nothing to reply to",
                ask.kind
            ));
        }
        if ask.status == "answered" {
            return Err(format!(
                "{id} was already answered by {}",
                ask.answered_by.as_deref().unwrap_or("?")
            ));
        }
        let now = now_ms();
        let waiting = now.saturating_sub(ask.waiter_seen_ms) < 5_000;
        if let Some(m) = self.message_mut(id) {
            m.status = "answered".into();
            m.answer = Some(text.to_string());
            m.answered_by = from.map(String::from);
            m.answered_ms = Some(now);
        }
        if let Some(asker_circle) = ask.from_pane.as_deref().and_then(|p| self.circle_of(p)) {
            self.record_contact(from, &asker_circle);
        }
        let mut delivered_to = None;
        if !waiting {
            if let Some(asker) = ask
                .from_pane
                .clone()
                .filter(|p| self.panes.iter().any(|x| &x.slug == p))
            {
                let rid = self.post_message(MessageRecord {
                    kind: "reply".into(),
                    from_pane: from.map(String::from),
                    from_circle: from.and_then(|p| self.circle_of(p)),
                    to_pane: asker.clone(),
                    text: text.to_string(),
                    reply_to: Some(id.to_string()),
                    ..Default::default()
                });
                delivered_to = Some(json!({"pane": asker, "message": rid}));
            }
        }
        Ok(json!({
            "id": id,
            "status": "answered",
            "to_waiting_caller": waiting,
            "asker": ask.from_pane,
            "pasted": delivered_to,
        }))
    }

    /// A circle was renamed: remember the old label (it keeps resolving) and
    /// tell every contact.
    pub(super) fn on_circle_renamed(&mut self, slug: &str, old: &str, new: &str) {
        if old == new {
            return;
        }
        let former = self
            .comms
            .former_labels
            .entry(slug.to_string())
            .or_default();
        former.retain(|l| l != new && l != old);
        former.push(old.to_string());
        let lead = self.effective_lead(slug).unwrap_or_else(|| "-".into());
        self.notify_contacts(
            slug,
            format!(
                "circle \"{old}\" (one you have talked to) is now called \"{new}\" \
                 (same circle: slug {slug}, lead pane {lead}). Use the new name; the old \
                 one still resolves for now."
            ),
        );
    }

    /// Pane `slug` (in `circle`) just closed. Re-route messages addressed to
    /// the circle, fail the rest, forget it as a contact, and tell the
    /// circle's contacts if it was the lead.
    pub(super) fn on_pane_closed(&mut self, slug: &str, circle: &str, was_lead: bool) {
        let new_lead = self.effective_lead(circle);
        let now = now_ms();
        for m in self.comms.messages.iter_mut() {
            if m.status != "pending" || m.to_pane != slug {
                continue;
            }
            match (&m.to_circle, &new_lead) {
                (Some(c), Some(l)) if c == circle => m.to_pane = l.clone(),
                _ => {
                    m.status = "failed".into();
                    m.note = Some(format!("pane {slug} closed before it was delivered"));
                }
            }
        }
        for m in self.comms.messages.iter_mut() {
            // A question this pane got but never answered: if it was asked of
            // the circle, the new lead gets it; otherwise it stays unanswered.
            if m.kind == "ask" && m.status == "delivered" && m.to_pane == slug {
                match (&m.to_circle, &new_lead) {
                    (Some(c), Some(l)) if c == circle => {
                        m.to_pane = l.clone();
                        m.status = "pending".into();
                        m.note = Some(format!("re-asked: {slug} closed without answering"));
                    }
                    _ => m.note = Some(format!("pane {slug} closed without answering ({now})")),
                }
            }
        }
        for set in self.comms.contacts.values_mut() {
            set.remove(slug);
        }
        self.comms.contacts.retain(|_, s| !s.is_empty());
        self.comms.born.remove(slug);
        if self.comms.leads.get(circle).map(String::as_str) == Some(slug) {
            self.comms.leads.remove(circle);
        }
        if !was_lead {
            return;
        }
        let label = self.workspace_label(circle);
        let text = match &new_lead {
            Some(l) => format!(
                "the lead pane of circle \"{label}\" ({slug}) closed; pane {l} leads it now. \
                 Messages to \"{label}\" go there."
            ),
            None => format!(
                "circle \"{label}\" closed (its last pane, {slug}, is gone). Messages to it \
                 will fail."
            ),
        };
        self.notify_contacts(circle, text);
        if new_lead.is_none() {
            self.comms.contacts.remove(circle);
        }
    }

    fn notify_contacts(&mut self, circle: &str, text: String) {
        let panes: Vec<String> = self
            .comms
            .contacts
            .get(circle)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        for p in panes {
            let alive_elsewhere = self
                .panes
                .iter()
                .any(|x| x.slug == p && x.workspace != circle);
            if !alive_elsewhere {
                continue;
            }
            self.post_message(MessageRecord {
                kind: "notice".into(),
                to_pane: p,
                text: text.clone(),
                about: Some(circle.to_string()),
                ..Default::default()
            });
        }
    }

    /// The circle's first-created terminal pane (earliest birth; panes older
    /// than the birth record count as oldest, in pane-list order). Unlike
    /// [`Self::effective_lead`], a `ctl lead` override does not apply.
    pub(crate) fn first_pane(&self, circle: &str) -> Option<String> {
        self.panes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.workspace == circle && p.kind != "file")
            .min_by_key(|(i, p)| (self.comms.born.get(&p.slug).copied().unwrap_or(0), *i))
            .map(|(_, p)| p.slug.clone())
    }

    /// Turn a host circle mode (host.json `circle_modes`, e.g. AFK) on or off
    /// for `circle`, and send the mode's prompt to the circle's first pane.
    /// No-op (no prompt) when it is already in that state. Returns the
    /// prompt's message id.
    pub(crate) fn set_circle_mode(
        &mut self,
        circle: &str,
        mode: &str,
        on: bool,
    ) -> Result<Option<String>, String> {
        self.set_circle_mode_with(&crate::host::circle_modes(), circle, mode, on)
    }

    /// [`Self::set_circle_mode`] against explicit mode defs (tests).
    pub(crate) fn set_circle_mode_with(
        &mut self,
        defs: &[crate::host::HostCircleMode],
        circle: &str,
        mode: &str,
        on: bool,
    ) -> Result<Option<String>, String> {
        let def = defs
            .iter()
            .find(|m| m.id == mode)
            .cloned()
            .ok_or_else(|| format!("no circle mode '{mode}' in host.json"))?;
        if !(if on {
            def.can_turn_on()
        } else {
            def.can_turn_off()
        }) {
            return Err(format!(
                "circle mode '{mode}' can't be turned {} from seance (host owns it)",
                if on { "on" } else { "off" }
            ));
        }
        let (slug, _) = self
            .resolve_circle(circle)
            .ok_or_else(|| format!("no circle '{circle}'"))?;
        let first = self
            .first_pane(&slug)
            .ok_or_else(|| format!("circle '{circle}' has no pane to tell"))?;
        let set = self.comms.modes.entry(slug.clone()).or_default();
        let changed = if on {
            set.insert(mode.to_string())
        } else {
            set.remove(mode)
        };
        if set.is_empty() {
            self.comms.modes.remove(&slug);
        }
        if !changed {
            return Ok(None);
        }
        if def.state_cmd.is_some() {
            self.comms.mode_pending.insert(
                (slug.clone(), mode.to_string()),
                (on, now_ms() + crate::host::MODE_PENDING_MS),
            );
        }
        events::log(
            "human",
            Some(&slug),
            Some(&first),
            "circle_mode",
            format!("{mode} {}", if on { "on" } else { "off" }),
        );
        let id = self.post_message(MessageRecord {
            kind: "prompt".into(),
            to_pane: first,
            to_circle: Some(slug),
            text: if on { def.on_prompt } else { def.off_prompt },
            ..Default::default()
        });
        Ok(Some(id))
    }

    /// Take a host `state_cmd` poll as the truth for `mode`: the circles it
    /// names, plus the circles of the panes it names, are in the mode and no
    /// others — except where a recent menu toggle is still pending and the
    /// host hasn't caught up. Returns whether anything visible changed.
    pub(crate) fn ingest_mode_state(
        &mut self,
        mode: &str,
        circles: &BTreeSet<String>,
        panes: &BTreeSet<String>,
        now_ms: u64,
    ) -> bool {
        let mut on: BTreeSet<String> = circles
            .iter()
            .filter_map(|c| self.resolve_circle(c).map(|(slug, _)| slug))
            .collect();
        on.extend(
            self.panes
                .iter()
                .filter(|p| panes.contains(&p.slug))
                .map(|p| p.workspace.clone()),
        );
        // Pending toggles: drop once confirmed or expired, else they win.
        self.comms
            .mode_pending
            .retain(|(circle, m), (want, until)| {
                m != mode || (*until > now_ms && on.contains(circle) != *want)
            });
        for ((circle, m), (want, _)) in &self.comms.mode_pending {
            if m == mode {
                if *want {
                    on.insert(circle.clone());
                } else {
                    on.remove(circle);
                }
            }
        }
        let before = self.comms.modes.clone();
        for set in self.comms.modes.values_mut() {
            set.remove(mode);
        }
        for circle in on {
            self.comms
                .modes
                .entry(circle)
                .or_default()
                .insert(mode.to_string());
        }
        self.comms.modes.retain(|_, set| !set.is_empty());
        self.comms.modes != before
    }

    /// Is a menu toggle of `mode` still waiting on the host's poll?
    pub(crate) fn mode_pending(&self, mode: &str) -> bool {
        self.comms.mode_pending.keys().any(|(_, m)| m == mode)
    }

    /// Message deliverability for a pane whose screen reads `activity`:
    /// Some(true) = paste now, Some(false) = wait, None = can never land.
    pub(super) fn message_can_land(
        &self,
        idx: usize,
        activity: seance_core::agent_state::Activity,
    ) -> Option<bool> {
        use seance_core::agent_state::Activity::*;
        match activity {
            Idle | Busy => Some(true),
            AwaitingInput | Limited => Some(false),
            Unknown if is_agent_command(&self.panes[idx].command) => Some(false),
            Unknown => None,
        }
    }

    /// JSON view of a message for ctl.
    pub(crate) fn message_json(&self, m: &MessageRecord) -> serde_json::Value {
        let mut v = serde_json::to_value(m).unwrap_or_default();
        v["to_circle_label"] = json!(m
            .to_circle
            .as_deref()
            .or(self.circle_of(&m.to_pane).as_deref())
            .map(|c| self.workspace_label(c)));
        v["from_circle_label"] = json!(m
            .from_pane
            .as_deref()
            .and_then(|p| self.circle_of(p))
            .map(|c| self.workspace_label(&c)));
        v
    }
}
