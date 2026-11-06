//! Session spawn/kill lifecycle: PTY setup, `SEANCE_*` env, persisted restore,
//! kill/reap, workspace fork + pane/workspace reorder.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use super::agent_session::{launch_with_session, new_session_uuid, split_session, Agent};
use super::helpers::shell_rc_path;
use super::{Engine, EnginePane, SpawnSpec, DEFAULT_COMMAND, DEFAULT_WORKSPACE};
use crate::events;
use crate::runtime::pty_session::{PtySession, SpawnConfig};
use crate::state::{new_identity_uuid, slugify, unique_slug, PersistedPane};

/// True when `command` launches the claude CLI — bare or by absolute path.
pub(super) fn is_claude_cmd(command: &str) -> bool {
    Agent::of(command) == Some(Agent::Claude)
}

impl Engine {
    /// Spawn a pane (terminal or file). Records its birth, which decides
    /// who leads a circle (comms.rs).
    pub fn spawn(&mut self, spec: SpawnSpec) -> Result<String> {
        let slug = self.spawn_pane(spec)?;
        self.note_born(&slug);
        Ok(slug)
    }

    fn spawn_pane(&mut self, spec: SpawnSpec) -> Result<String> {
        let name = if spec.name.trim().is_empty() {
            "session".into()
        } else {
            spec.name.trim().to_string()
        };
        let taken: Vec<&str> = self.panes.iter().map(|p| p.slug.as_str()).collect();
        let slug = unique_slug(&name, &taken);
        let pane_id = new_identity_uuid();
        // A spawn target may be given as a slug OR a label; an unknown name
        // mints a new circle (slug from the name, name kept as the label).
        let workspace = match spec.workspace.filter(|w| !w.trim().is_empty()) {
            Some(w) => self.workspace_slug_for_spawn(w.trim()),
            None => self
                .selected_workspace
                .clone()
                .unwrap_or_else(|| DEFAULT_WORKSPACE.into()),
        };
        self.ensure_workspace_id(&workspace);
        // New / unlisted workspace names land at the bottom of the sidebar,
        // never alphabetically at the top.
        if !self.workspace_order.iter().any(|w| w == &workspace) {
            self.workspace_order.push(workspace.clone());
        }
        let cwd_raw = spec.cwd.unwrap_or_else(|| "~".into());
        self.record_event(
            &slug,
            seance_core::replay::ReplayEvent::Spawned {
                name: name.clone(),
                workspace: workspace.clone(),
                command: spec.command.clone().unwrap_or_default(),
            },
        );
        let scratch_path = self.store.path_for(&slug);

        // Insert after the last pane of this workspace so the sidebar/tiles
        // show newest at the bottom of the group (not global-list quirks).
        let insert_at = self
            .panes
            .iter()
            .rposition(|p| p.workspace == workspace)
            .map(|i| i + 1)
            .unwrap_or(self.panes.len());

        if let Some(file) = spec.file {
            let path = PathBuf::from(shellexpand::tilde(&file).into_owned());
            self.panes.insert(
                insert_at,
                EnginePane {
                    pane_id: pane_id.clone(),
                    kind: "file".into(),
                    name,
                    slug: slug.clone(),
                    workspace,
                    cwd: cwd_raw,
                    command: path.to_string_lossy().to_string(),
                    tiled: spec.tiled,
                    resume_on_restore: false,
                    agent_session: None,
                    asleep: false,
                    scratch_path,
                    file: Some(path.to_string_lossy().to_string()),
                    session: None,
                    agency: crate::agency::Agency::default(),
                },
            );
            events::log(
                "daemon",
                None,
                Some(&slug),
                "pane_spawned",
                "file pane".into(),
            );
            return Ok(slug);
        }

        let explicit = spec.command.filter(|c| !c.trim().is_empty());
        let mut command = match &explicit {
            Some(c) => c.clone(),
            None => {
                let rc = shell_rc_path();
                if rc.is_file() {
                    format!("bash --init-file {}", rc.to_string_lossy())
                } else {
                    DEFAULT_COMMAND.into()
                }
            }
        };
        // Own the conversation: mint the session id here rather than letting
        // the agent pick one, so a pane can be put back on its exact session
        // after a crash. `--continue` is deliberately NOT the restore path — it
        // resumes the most recent conversation *in the cwd*, so panes sharing a
        // cwd (the normal case) would all land on the same one. codex can't be
        // given an id; the daemon discovers it (`agent_session.rs`).
        let (base, mut agent_session) = split_session(&command);
        if let Some(id) = &agent_session {
            command = launch_with_session(&base, &cwd_raw, id);
        } else if Agent::of(&command).is_some_and(Agent::mints) && !command.contains("--continue") {
            let id = new_session_uuid();
            command = format!("{command} --session-id {id}");
            agent_session = Some(id);
        } else if spec.resume && is_claude_cmd(&command) && !command.contains("--continue") {
            command = format!("{command} --continue");
        }

        let session =
            self.spawn_terminal_session(&slug, &command, &cwd_raw, &workspace, &pane_id)?;

        self.panes.insert(
            insert_at,
            EnginePane {
                pane_id,
                kind: "terminal".into(),
                name,
                slug: slug.clone(),
                workspace: workspace.clone(),
                cwd: cwd_raw,
                command: explicit
                    .map(|_| base)
                    .unwrap_or_else(|| DEFAULT_COMMAND.into()),
                tiled: spec.tiled,
                resume_on_restore: spec.resume,
                agent_session,
                asleep: false,
                scratch_path,
                file: None,
                session: Some(session),
                agency: crate::agency::Agency::default(),
            },
        );
        events::log(
            "daemon",
            Some(&workspace),
            Some(&slug),
            "pane_spawned",
            "terminal pane".into(),
        );
        Ok(slug)
    }

    pub(super) fn spawn_from_persisted(&mut self, p: &PersistedPane) -> Result<()> {
        self.ensure_workspace_id(&p.workspace);
        // Spawn with the persisted name; if slug collides, unique_slug suffixes.
        // Prefer exact slug restore when free.
        let taken: Vec<&str> = self.panes.iter().map(|x| x.slug.as_str()).collect();
        let want_slug = if taken.contains(&p.slug.as_str()) {
            unique_slug(&p.name, &taken)
        } else {
            p.slug.clone()
        };

        if p.kind == "file" {
            let path = PathBuf::from(shellexpand::tilde(&p.command).into_owned());
            self.panes.push(EnginePane {
                pane_id: p.pane_id.clone(),
                kind: "file".into(),
                name: p.name.clone(),
                slug: want_slug,
                workspace: p.workspace.clone(),
                cwd: p.cwd.clone(),
                command: p.command.clone(),
                tiled: p.tiled,
                resume_on_restore: false,
                agent_session: None,
                asleep: p.asleep,
                scratch_path: self.store.path_for(&p.slug),
                file: Some(path.to_string_lossy().to_string()),
                session: None,
                agency: crate::agency::Agency::default(),
            });
            return Ok(());
        }

        // Put the pane back on the exact conversation it had. Falls back to the
        // legacy `--continue` only for claude panes persisted before session
        // ids were minted (no id on record).
        let (base, explicit) = split_session(&p.command);
        let mut agent_session = p.agent_session.clone().or(explicit);
        let mut command = match &agent_session {
            Some(id) => launch_with_session(&base, &p.cwd, id),
            None => base.clone(),
        };
        if agent_session.is_none()
            && p.resume_on_restore
            && is_claude_cmd(&command)
            && !command.contains("--continue")
        {
            command = format!("{command} --continue");
        }
        // A pane restored from a pre-session-id state file adopts one now, so
        // the *next* crash is recoverable even if this one wasn't.
        if agent_session.is_none()
            && Agent::of(&command).is_some_and(Agent::mints)
            && !command.contains("--continue")
        {
            let id = new_session_uuid();
            command = format!("{command} --session-id {id}");
            agent_session = Some(id);
        }
        if command == DEFAULT_COMMAND || command.starts_with("bash") {
            let rc = shell_rc_path();
            if rc.is_file() && !command.contains("--init-file") {
                command = format!("bash --init-file {}", rc.to_string_lossy());
            }
        }

        // A circle that was asleep stays asleep across a daemon restart —
        // relaunching it here would defeat the point (it is not holding RAM,
        // and nobody asked for it back).
        let session = if p.asleep {
            None
        } else {
            Some(self.spawn_terminal_session(
                &want_slug,
                &command,
                &p.cwd,
                &p.workspace,
                &p.pane_id,
            )?)
        };
        self.panes.push(EnginePane {
            pane_id: p.pane_id.clone(),
            kind: "terminal".into(),
            name: p.name.clone(),
            slug: want_slug,
            workspace: p.workspace.clone(),
            cwd: p.cwd.clone(),
            command: base,
            tiled: p.tiled,
            resume_on_restore: p.resume_on_restore,
            agent_session,
            asleep: p.asleep,
            scratch_path: self.store.path_for(&p.slug),
            file: None,
            session,
            agency: crate::agency::Agency::default(),
        });
        Ok(())
    }

    pub(super) fn spawn_terminal_session(
        &self,
        slug: &str,
        command: &str,
        cwd_raw: &str,
        workspace: &str,
        pane_id: &str,
    ) -> Result<PtySession> {
        let mut cwd = PathBuf::from(shellexpand::tilde(cwd_raw).into_owned());
        // A missing cwd (config typo, path from another machine) must not turn
        // the spawn into a silent dead click — fall back to home, loudly.
        if !cwd.is_dir() {
            eprintln!(
                "[seance daemon] spawn '{slug}': cwd {} missing — falling back to ~",
                cwd.display()
            );
            cwd = PathBuf::from(shellexpand::tilde("~").into_owned());
        }
        let scratch_path = self.store.path_for(slug);
        let mut env = HashMap::new();
        env.insert("SEANCE_SESSION".into(), slug.to_string());
        env.insert("SEANCE_PANE_ID".into(), pane_id.to_string());
        env.insert(
            "SEANCE_WORKSPACE_ID".into(),
            self.workspace_ids[workspace].clone(),
        );
        // The circle's human-readable slug. This is the whole reason the slug never
        // moves: a rename cannot reach into a running process's environment,
        // so the value it was given at spawn has to stay true forever.
        env.insert("SEANCE_WORKSPACE".into(), workspace.to_string());
        env.insert(
            "SEANCE_WORKSPACE_NAME".into(),
            self.workspace_label(workspace),
        );
        env.insert(
            "SEANCE_SCRATCHPAD".into(),
            scratch_path.to_string_lossy().to_string(),
        );
        env.insert(
            "SEANCE_SOCKET".into(),
            crate::control::bind_socket_path()
                .to_string_lossy()
                .to_string(),
        );
        PtySession::spawn(
            slug.to_string(),
            SpawnConfig {
                command: crate::agents::isolate_codex(command),
                cwd,
                env,
                cols: 100,
                rows: 30,
            },
            self.event_tx.clone(),
        )
    }

    pub fn kill_pane(&mut self, slug: &str) {
        if let Some(idx) = self.panes.iter().position(|p| p.slug == slug) {
            let circle = self.panes[idx].workspace.clone();
            let was_lead = self.effective_lead(&circle).as_deref() == Some(slug);
            let mut pane = self.panes.remove(idx);
            let workspace = pane.workspace.clone();
            if let Some(s) = pane.session.take() {
                s.shutdown();
            }
            self.cmd_log.remove_pane(slug);
            self.statuses.remove(slug);
            self.pane_busy.remove(slug);
            self.pane_busy_at.remove(slug);
            self.pane_woke_at.remove(slug);
            self.wake_failed_at.remove(slug);
            if let Ok(m) = self.last_spinner.get_mut() {
                m.remove(slug);
            }
            // Its conversation record (auto-sleep) goes with it.
            if let Some(path) = super::sleep::hosted_path(&pane.pane_id) {
                let _ = std::fs::remove_file(path);
            }
            self.pane_parents.remove(slug);
            self.drop_pane_tasks(slug);
            // Slugs get reused. A window still holding this pane's frame as a
            // damage base would otherwise apply the next pane's rows onto it.
            self.invalidate_bases(slug);
            if self.focused_pane.as_deref() == Some(slug) {
                self.focused_pane = self.panes.first().map(|p| p.slug.clone());
            }
            // Last pane gone and nobody created this circle on purpose → drop
            // the row (order/clocks/subscriptions) instead of leaving
            // an empty one in both sidebars.
            // Before the prune: notices name the circle by its label.
            self.on_pane_closed(slug, &circle, was_lead);
            self.prune_workspace_if_empty(&workspace);
            events::log("daemon", None, Some(slug), "pane_killed", "killed".into());
        }
    }

    /// Move `slug` into `workspace`, inserting immediately before `before`
    /// (another slug) or appending when `before` is None / missing. Pane-list
    /// order is the persistence key for sidebar + tile layout.
    pub fn reorder_pane(&mut self, slug: &str, workspace: &str, before: Option<&str>) {
        if Some(slug) == before {
            return;
        }
        let Some(from_idx) = self.panes.iter().position(|p| p.slug == slug) else {
            return;
        };
        let mut pane = self.panes.remove(from_idx);
        pane.workspace = slugify(workspace);
        self.ensure_workspace_id(&pane.workspace);
        let insert_at = before
            .and_then(|b| self.panes.iter().position(|p| p.slug == b))
            .unwrap_or(self.panes.len());
        events::log(
            "human",
            Some(workspace),
            Some(slug),
            "pane_moved",
            format!(
                "moved '{}' into {} (reorder{})",
                pane.name,
                pane.workspace,
                before.map(|b| format!(" before {b}")).unwrap_or_default()
            ),
        );
        self.panes.insert(insert_at, pane);
        self.selected_workspace = Some(slugify(workspace));
    }

    /// Place workspace `moved` immediately before `before` in the sidebar
    /// order. Builds the full display order (explicit + any extras) so a
    /// partial `workspace_order` still ends up consistent.
    pub fn reorder_workspace(&mut self, moved: &str, before: &str) {
        if moved == before {
            return;
        }
        // Full ordered list: preferred order first, then any workspaces not
        // yet listed (extras then pane order — not alphabetical).
        let mut order = self.workspace_order.clone();
        let mut seen: std::collections::HashSet<String> = order.iter().cloned().collect();
        for w in self
            .extra_workspaces
            .iter()
            .chain(self.panes.iter().map(|p| &p.workspace))
        {
            if seen.insert(w.clone()) {
                order.push(w.clone());
            }
        }
        order.retain(|w| w != moved);
        let idx = order
            .iter()
            .position(|w| w == before)
            .unwrap_or(order.len());
        order.insert(idx, moved.to_string());
        self.workspace_order = order;
        events::log(
            "human",
            Some(moved),
            None,
            "workspace_reordered",
            format!("workspace '{moved}' before '{before}'"),
        );
    }
}

#[cfg(test)]
mod session_id_tests {
    use super::*;

    #[test]
    fn claude_detected_bare_and_by_path() {
        assert!(is_claude_cmd("claude --dangerously-skip-permissions"));
        assert!(is_claude_cmd("/home/you/.local/bin/claude --resume abc"));
        assert!(!is_claude_cmd("bash -l"));
        assert!(!is_claude_cmd("claude-agent-acp"));
    }
}
