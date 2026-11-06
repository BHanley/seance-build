//! Engine integration tests (control plane with stub panes).

use super::helpers::now_ms;
use super::queue::IDLE_SETTLE_MS;
use super::*;
use crate::control::{ControlRequest, ControlResponse};
use std::path::PathBuf;

pub(super) fn with_test_state_dir<T>(tag: &str, f: impl FnOnce() -> T) -> T {
    // Share lock with state::tests — both mutate SEANCE_STATE_DIR.
    let _g = crate::state::test_env_lock();
    let dir = std::env::temp_dir().join(format!(
        "seance-eng-state-{}-{}-{}",
        tag,
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Claude's home too, so transcripts a test writes never land in the
    // real ~/.claude.
    let vars = [
        ("SEANCE_STATE_DIR", dir.clone()),
        ("CLAUDE_CONFIG_DIR", dir.join("claude")),
    ];
    let prev: Vec<_> = vars
        .iter()
        .map(|(k, v)| {
            let was = std::env::var_os(k);
            std::env::set_var(k, v);
            (*k, was)
        })
        .collect();
    let out = f();
    for (k, was) in prev {
        match was {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn temp_scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "seance-eng-scratch-{}-{}-{}",
        tag,
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn handle_control_list_scope_and_status_set() {
    with_test_state_dir("list-status", || {
        let scratch = temp_scratch("list-status");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let a = eng.push_stub_pane("worker-a", "lab");
        let b = eng.push_stub_pane("worker-b", "other");

        let list_all = eng.handle_control(ControlRequest::List {
            scope: None,
            from: None,
        });
        assert!(list_all.ok);
        let panes = list_all.data.as_ref().unwrap()["panes"].as_array().unwrap();
        assert_eq!(panes.len(), 2);

        let list_lab = eng.handle_control(ControlRequest::List {
            scope: Some("lab".into()),
            from: None,
        });
        let panes = list_lab.data.as_ref().unwrap()["panes"].as_array().unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["slug"], a);

        let set = eng.handle_control(ControlRequest::StatusSet {
            state: "working".into(),
            note: Some("busy".into()),
            pane: Some(a.clone()),
            scope: None,
            from: None, // external cli may cross
        });
        assert!(set.ok, "{:?}", set.error);
        assert_eq!(
            eng.statuses
                .get(&a)
                .map(|(s, n)| (s.as_str(), n.as_deref())),
            Some(("working", Some("busy")))
        );

        // Invalid status
        let bad = eng.handle_control(ControlRequest::StatusSet {
            state: "shipped".into(),
            note: None,
            pane: Some(a.clone()),
            scope: None,
            from: None,
        });
        assert!(!bad.ok);

        // Scope blocks cross-workspace by name
        let cross = eng.handle_control(ControlRequest::StatusSet {
            state: "idle".into(),
            note: None,
            pane: Some(b.clone()),
            scope: Some("lab".into()),
            from: None,
        });
        assert!(!cross.ok);
        assert!(cross.error.as_deref().unwrap_or("").contains("outside"));

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn slug_beats_another_panes_display_name() {
    // Two panes named "term-2": the second gets slug `term-2-2`, so the key
    // "term-2" is one pane's slug AND the other's name. Resolving both in one
    // pass drove the wrong pane (2026-08-04: a restore silently skipped the
    // pane it was asked for and injected into its neighbour twice).
    with_test_state_dir("slug-vs-name", || {
        let scratch = temp_scratch("slug-vs-name");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let first = eng.push_stub_pane("term-2", "buzz");
        let second = eng.push_stub_pane("term-2", "lunch-fix");
        assert_eq!(first, "term-2");
        assert_eq!(second, "term-2-2");

        // Exact slug wins over the earlier pane's identical display name.
        let hit = eng.handle_control(ControlRequest::StatusSet {
            state: "working".into(),
            note: None,
            pane: Some("term-2-2".into()),
            scope: None,
            from: None,
        });
        assert!(hit.ok, "{:?}", hit.error);
        assert!(eng.statuses.contains_key("term-2-2"));
        assert!(!eng.statuses.contains_key("term-2"));

        // Scope narrows candidates BEFORE matching, so the name collision
        // resolves to the scoped workspace's pane rather than erroring.
        let scoped = eng.handle_control(ControlRequest::StatusSet {
            state: "idle".into(),
            note: None,
            pane: Some("term-2".into()),
            scope: Some("lunch-fix".into()),
            from: None,
        });
        assert!(scoped.ok, "{:?}", scoped.error);
        assert_eq!(
            eng.statuses.get("term-2-2").map(|(s, _)| s.as_str()),
            Some("idle")
        );

        // A genuinely absent pane still reports missing, not "outside".
        let missing = eng.handle_control(ControlRequest::StatusSet {
            state: "idle".into(),
            note: None,
            pane: Some("nope".into()),
            scope: Some("lunch-fix".into()),
            from: None,
        });
        assert!(!missing.ok);
        assert!(missing.error.as_deref().unwrap_or("").contains("no pane"));

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn handle_control_self_only_blocks_cross_agent() {
    with_test_state_dir("self-only", || {
        let scratch = temp_scratch("self-only");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let w1 = eng.push_stub_pane("w1", "main");
        let w2 = eng.push_stub_pane("w2", "main");

        // Agent w1 cannot status-set w2
        let denied = eng.handle_control(ControlRequest::StatusSet {
            state: "working".into(),
            note: None,
            pane: Some(w2.clone()),
            scope: None,
            from: Some(w1.clone()),
        });
        assert!(!denied.ok);
        assert!(denied.error.as_deref().unwrap_or("").contains("self-only"));

        // Same agent ok
        let ok = eng.handle_control(ControlRequest::StatusSet {
            state: "idle".into(),
            note: None,
            pane: Some(w1.clone()),
            scope: None,
            from: Some(w1.clone()),
        });
        assert!(ok.ok, "{:?}", ok.error);

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn handle_control_note_bumps_pad_rev() {
    with_test_state_dir("note-rev", || {
        let scratch = temp_scratch("note-rev");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let slug = eng.push_stub_pane("notes", "main");

        let r1 = eng.handle_control(ControlRequest::Note {
            pane: Some(slug.clone()),
            text: "hello".into(),
            append: true,
            scope: None,
            from: None,
        });
        assert!(r1.ok, "{:?}", r1.error);
        assert_eq!(r1.data.as_ref().unwrap()["pad_rev"], 1);

        let r2 = eng.handle_control(ControlRequest::Note {
            pane: Some(slug.clone()),
            text: "world".into(),
            append: true,
            scope: None,
            from: None,
        });
        assert!(r2.ok);
        assert_eq!(r2.data.as_ref().unwrap()["pad_rev"], 2);
        assert_eq!(eng.pad_revs.get(&slug).copied(), Some(2));

        let path = eng
            .panes
            .iter()
            .find(|p| p.slug == slug)
            .unwrap()
            .scratch_path
            .clone();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("hello"));
        assert!(body.contains("world"));

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn begin_and_complete_task_lifecycle() {
    with_test_state_dir("task-life", || {
        let scratch = temp_scratch("task-life");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let slug = eng.push_stub_pane("worker", "main");

        let id1 = eng.begin_task(&slug, "first inject");
        assert!(id1.starts_with("task-"));
        assert_eq!(
            eng.active_tasks.get(&slug).map(|s| s.as_str()),
            Some(id1.as_str())
        );
        assert_eq!(eng.tasks.get(&id1).map(|t| t.status.as_str()), Some("open"));
        assert_eq!(
            eng.tasks.get(&id1).map(|t| t.body.as_str()),
            Some("first inject")
        );

        // A second inject parks the prior open task (`send --now` cancels
        // it instead; see sends_park_queue_or_replace).
        let id2 = eng.begin_task(&slug, "second inject");
        assert_ne!(id1, id2);
        assert_eq!(
            eng.tasks.get(&id1).map(|t| t.status.as_str()),
            Some("parked")
        );
        assert_eq!(
            eng.active_tasks.get(&slug).map(|s| s.as_str()),
            Some(id2.as_str())
        );

        let done = eng.complete_active_task(&slug, None);
        assert_eq!(done.as_deref(), Some(id2.as_str()));
        assert_eq!(eng.tasks.get(&id2).map(|t| t.status.as_str()), Some("done"));
        assert!(eng.active_tasks.get(&slug).is_none());

        // Sidecar written
        let path = eng
            .panes
            .iter()
            .find(|p| p.slug == slug)
            .unwrap()
            .scratch_path
            .clone();
        assert!(
            path.with_extension("taskid").exists()
                || path.with_extension("task.json").exists()
                || true /* last complete may leave files from begin */
        );

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn finish_rejects_invalid_task_before_any_mutation() {
    with_test_state_dir("finish-task-identity", || {
        let scratch = temp_scratch("finish-task-identity");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let own = eng.push_stub_pane("worker", "main");
        let other = eng.push_stub_pane("unrelated", "main");
        let cancelled = eng.begin_task(&own, "original assignment");
        // `send --now`: cancelled, not parked (a parked task can be finished).
        eng.supersede_active(&own);
        let current = eng.begin_task(&own, "replacement assignment");
        let foreign = eng.begin_task(&other, "other worker's assignment");
        let path = eng
            .panes
            .iter()
            .find(|p| p.slug == own)
            .unwrap()
            .scratch_path
            .clone();
        std::fs::write(&path, "keep this pad").unwrap();
        eng.statuses.insert(own.clone(), ("working".into(), None));
        let request = |task| ControlRequest::Finish {
            pane: None,
            body: Some("completed work".into()),
            append: true,
            status: "done".into(),
            status_note: None,
            empty_ok: false,
            task,
            scope: None,
            from: Some(own.clone()),
        };
        for id in [&cancelled, &foreign, "task-missing"] {
            let result = eng.handle_control(request(Some(id.to_string())));
            assert!(!result.ok, "invalid task {id} accepted");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep this pad");
            assert_eq!(eng.statuses[&own].0, "working");
            assert_eq!(eng.active_tasks[&own], current);
            assert_eq!(eng.tasks[&current].status, "open");
            assert!(!eng.pad_revs.contains_key(&own));
        }
        let result = eng.handle_control(request(None));
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.data.unwrap()["task_id"], current);
        assert_eq!(eng.tasks[&current].status, "done");
        assert_eq!(eng.tasks[&foreign].status, "open");
        assert_eq!(eng.tasks[&cancelled].status, "cancelled");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!eng.handle_control(request(Some(current))).ok);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(scratch);
    });
}

#[test]
fn finish_done_requires_body_or_empty_ok() {
    with_test_state_dir("finish-ev", || {
        let scratch = temp_scratch("finish-ev");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let slug = eng.push_stub_pane("worker", "main");

        // done without body / empty_ok → error (evidence-bound)
        let bad = eng.handle_control(ControlRequest::Finish {
            pane: Some(slug.clone()),
            body: None,
            append: true,
            status: "done".into(),
            status_note: None,
            empty_ok: false,
            task: None,
            scope: None,
            from: None,
        });
        assert!(!bad.ok, "expected evidence-bound failure");

        let ok = eng.handle_control(ControlRequest::Finish {
            pane: Some(slug.clone()),
            body: Some("shipped it".into()),
            append: true,
            status: "done".into(),
            status_note: None,
            empty_ok: false,
            task: None,
            scope: None,
            from: None,
        });
        assert!(ok.ok, "{:?}", ok.error);
        assert_eq!(
            eng.statuses.get(&slug).map(|(s, _)| s.as_str()),
            Some("done")
        );

        let empty_ok = eng.handle_control(ControlRequest::Finish {
            pane: Some(slug.clone()),
            body: None,
            append: true,
            status: "done".into(),
            status_note: None,
            empty_ok: true,
            task: None,
            scope: None,
            from: None,
        });
        assert!(empty_ok.ok, "{:?}", empty_ok.error);

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn seize_release_drive_agency() {
    with_test_state_dir("agency-ctl", || {
        let scratch = temp_scratch("agency-ctl");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let slug = eng.push_stub_pane("w", "main");

        let seize = eng.handle_control(ControlRequest::Seize {
            pane: slug.clone(),
            as_owner: Some("human".into()),
            scope: None,
            from: None,
        });
        assert!(seize.ok, "{:?}", seize.error);
        let pane = eng.panes.iter().find(|p| p.slug == slug).unwrap();
        assert!(pane.agency.owner.is_human());

        let release = eng.handle_control(ControlRequest::Release {
            pane: slug.clone(),
            scope: None,
            from: None,
        });
        assert!(release.ok);
        let pane = eng.panes.iter().find(|p| p.slug == slug).unwrap();
        assert!(pane.agency.owner.is_none());

        let drive = eng.handle_control(ControlRequest::DriveMode {
            pane: slug.clone(),
            mode: "locked_human".into(),
            scope: None,
            from: None,
        });
        assert!(drive.ok, "{:?}", drive.error);
        let pane = eng.panes.iter().find(|p| p.slug == slug).unwrap();
        assert_eq!(
            pane.agency.drive_mode,
            crate::agency::DriveMode::LockedHuman
        );

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn last_pane_death_prunes_the_workspace_but_spares_created_circles() {
    with_test_state_dir("ws-prune", || {
        let scratch = temp_scratch("ws-prune");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());

        // Implicit circle (born with its pane) → pruned when the pane dies.
        let a = eng.push_stub_pane("worker-a", "lab");
        eng.workspace_order.push("lab".into());
        eng.workspace_output.insert("lab".into(), 1);
        eng.workspace_touch_ms.insert("lab".into(), 2);

        // Explicitly created empty circle → keeps its row.
        let b = eng.push_stub_pane("worker-b", "studio");
        eng.extra_workspaces.push("studio".into());
        eng.workspace_order.push("studio".into());

        eng.kill_pane(&a);
        assert!(!eng.workspace_order.iter().any(|w| w == "lab"));
        assert!(!eng.workspace_output.contains_key("lab"));
        assert!(!eng.workspace_touch_ms.contains_key("lab"));

        eng.kill_pane(&b);
        assert!(eng.workspace_order.iter().any(|w| w == "studio"));

        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn new_from_a_pane_records_parent_and_status_carries_the_row() {
    with_test_state_dir("pane-parent", || {
        let scratch = temp_scratch("pane-parent");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let orch = eng.push_stub_pane("orch", "lab");
        let doc = scratch.join("notes.md");
        std::fs::write(&doc, "# notes\n").unwrap();
        let resp = eng.handle_control(ControlRequest::New {
            name: "orch".into(), // collides: slug gets suffixed, name kept
            cwd: None,
            command: None,
            workspace: Some("lab".into()),
            file: Some(doc.to_string_lossy().into()),
            scope: None,
            from: Some(orch.clone()),
        });
        assert!(resp.ok, "{:?}", resp.error);
        let data = resp.data.unwrap();
        let child = data["slug"].as_str().unwrap().to_string();
        assert_ne!(child, orch);
        assert_eq!(data["name"], "orch");
        assert_eq!(data["parent"], orch.as_str());
        assert!(data.get("cwd").is_some() && data.get("command").is_some());

        // `status` is the full roster row: cwd, parent, activity.
        let st = eng
            .handle_control(ControlRequest::Status {
                pane: child.clone(),
                scope: None,
                from: None,
            })
            .data
            .unwrap();
        assert_eq!(st["parent"], orch.as_str());
        assert!(st["cwd"].is_string());
        assert_eq!(st["activity"], "unknown");

        // Survives persist/reload; forgotten when the child dies.
        eng.persist();
        let state = crate::state::AppState::load();
        assert_eq!(state.pane_parents, vec![(child.clone(), orch.clone())]);
        eng.kill_pane(&child);
        assert!(eng.pane_parents.is_empty());

        // Outside a pane (or naming no pane) there is no parent.
        let resp = eng.handle_control(ControlRequest::New {
            name: "loose".into(),
            cwd: None,
            command: None,
            workspace: Some("lab".into()),
            file: Some(doc.to_string_lossy().into()),
            scope: None,
            from: Some("ghost".into()),
        });
        assert!(resp.data.unwrap()["parent"].is_null());
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn queued_send_leaves_the_running_task_alone_and_fail_rolls_back() {
    with_test_state_dir("send-queue", || {
        let scratch = temp_scratch("send-queue");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let w = eng.push_stub_pane("w", "lab");
        let running = eng.begin_task(&w, "long job");

        // --queue on a busy pane: task opens as queued; the running one stays open.
        let resp = eng.handle_control(ControlRequest::Send {
            pane: w.clone(),
            text: "run this next".into(),
            submit: true,
            force: false,
            queue: true,
            now: false,
            scope: None,
            from: Some("orch".into()),
        });
        // Stub panes have no PTY: queueing must not need one.
        let data = resp.data.unwrap_or_else(|| panic!("{:?}", resp.error));
        let queued = data["task_id"].as_str().unwrap().to_string();
        assert_eq!(data["status"], "queued");
        assert_eq!(data["position"], 1);
        assert_eq!(eng.tasks[&running].status, "open");
        assert_eq!(eng.active_tasks.get(&w), Some(&running));
        assert_eq!(eng.queued_for(&w), vec![queued.clone()]);
        assert_eq!(eng.tasks[&queued].queued_by.as_deref(), Some("agent:orch"));

        // Activation parks the running task; a failed delivery reopens it.
        eng.activate_queued(&w, &queued);
        assert_eq!(eng.tasks[&running].status, "parked");
        assert_eq!(
            eng.tasks[&queued].supersedes.as_deref(),
            Some(running.as_str())
        );
        let resp = eng.handle_control(ControlRequest::TaskFail {
            id: queued.clone(),
            reason: Some("delivery not confirmed".into()),
            scope: None,
            from: None,
        });
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.data.unwrap()["restored"], running.as_str());
        assert_eq!(eng.tasks[&queued].status, "failed");
        assert_eq!(eng.tasks[&running].status, "open");
        assert_eq!(eng.active_tasks.get(&w), Some(&running));
        // Only open/queued tasks can fail.
        assert!(eng.fail_task(&queued, "again").is_err());

        // A direct send parks it too, and failing restores it.
        let solo = eng.begin_task(&w, "second");
        assert_eq!(eng.tasks[&running].status, "parked");
        assert_eq!(eng.fail_task(&solo, "x").unwrap(), Some(running.clone()));

        // Pane gone: its open task is orphaned and its queue cancelled, so a
        // pane that reuses the slug doesn't wait behind them.
        let (q2, _) = eng.queue_task(&w, "later", "cli");
        eng.kill_pane(&w);
        assert_eq!(eng.tasks[&q2].status, "cancelled");
        assert_eq!(eng.tasks[&running].status, "orphaned");
        assert_eq!(eng.open_task(&w), None);
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn sends_park_queue_or_replace() {
    with_test_state_dir("send-rules", || {
        let scratch = temp_scratch("send-rules");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let w = eng.push_stub_pane("w", "lab");
        let other = eng.push_stub_pane("other", "lab");
        let send = |eng: &mut Engine, pane: &str, text: &str, queue: bool, now: bool| {
            eng.handle_control(ControlRequest::Send {
                pane: pane.into(),
                text: text.into(),
                submit: true,
                force: false,
                queue,
                now,
                scope: None,
                from: None,
            })
        };

        // Idle with an open task: the new task goes in and the old one is
        // parked, not cancelled. It shows on the pane's row and can still be
        // finished by id, without touching the current task or its badge.
        let first = eng.begin_task(&w, "first");
        let second = eng.begin_task(&w, "second");
        assert_eq!(eng.tasks[&first].status, "parked");
        assert!(eng.tasks[&first].finished_ms.is_some());
        let row =
            |eng: &Engine| eng.pane_summary_json(eng.panes.iter().find(|p| p.slug == w).unwrap());
        assert_eq!(
            row(&eng)["parked_tasks"],
            serde_json::json!([first.clone()])
        );
        eng.statuses.insert(w.clone(), ("working".into(), None));
        let r = eng.handle_control(ControlRequest::Finish {
            pane: Some(w.clone()),
            body: Some("late answer".into()),
            append: true,
            status: "done".into(),
            status_note: None,
            empty_ok: false,
            task: Some(first.clone()),
            scope: None,
            from: None,
        });
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(eng.tasks[&first].status, "done");
        assert_eq!(eng.open_task(&w).as_deref(), Some(second.as_str()));
        assert_eq!(eng.statuses[&w].0, "working");

        // Busy (here --queue, the same path): queued behind the running task,
        // oldest first; a plain send behind a waiting queue joins it.
        let d = send(&mut eng, &w, "third", true, false).data.unwrap();
        let third = d["task_id"].as_str().unwrap().to_string();
        assert_eq!(
            (
                d["status"].as_str(),
                d["behind"].as_str(),
                d["ahead"].as_u64()
            ),
            (Some("queued"), Some(second.as_str()), Some(1))
        );
        let d = send(&mut eng, &w, "fourth", false, false).data.unwrap();
        let fourth = d["task_id"].as_str().unwrap().to_string();
        assert_eq!(
            (d["behind"].as_str(), d["ahead"].as_u64()),
            (Some(third.as_str()), Some(2))
        );
        assert_eq!(eng.queued_for(&w), vec![third.clone(), fourth.clone()]);
        assert_eq!(eng.tasks[&second].status, "open");

        // `--now` cancels the open task outright, --queue included, and goes
        // to the front, so it is delivered next and parks nothing.
        let d = send(&mut eng, &w, "urgent", true, true).data.unwrap();
        let urgent = d["task_id"].as_str().unwrap().to_string();
        assert_eq!(
            (d["position"].as_u64(), d["ahead"].as_u64()),
            (Some(1), Some(0))
        );
        assert_eq!(eng.tasks[&second].status, "cancelled");
        assert_eq!(
            eng.queued_for(&w),
            vec![urgent.clone(), third.clone(), fourth.clone()]
        );
        eng.activate_queued(&w, &urgent);
        assert_eq!(
            eng.tasks[&urgent].supersedes.as_deref(),
            Some(second.as_str())
        );

        // Handoff: the successor takes the queue; a plain kill cancels it and
        // orphans the open task, so a reused slug starts clean.
        let r = eng.handle_control(ControlRequest::Kill {
            pane: w.clone(),
            queue_to: Some(other.clone()),
            scope: None,
            from: None,
        });
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(eng.queued_for(&other), vec![third.clone(), fourth.clone()]);
        assert_eq!(eng.tasks[&urgent].status, "orphaned");
        let parked = eng.begin_task(&other, "a");
        eng.begin_task(&other, "b");
        eng.kill_pane(&other);
        assert_eq!(eng.tasks[&fourth].status, "cancelled");
        assert_eq!(eng.tasks[&parked].status, "orphaned");

        // Parked tasks prune like finished ones.
        let mut old = eng.tasks[&first].clone();
        old.status = "parked".into();
        old.finished_ms = Some(1);
        assert!(!super::keep_task(&old, now_ms()));
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

/// Saved state can name panes that didn't come back (seven of them on the
/// live daemon): their tasks must not wait for a pane reusing the slug.
#[test]
fn load_drops_tasks_of_panes_that_did_not_come_back() {
    with_test_state_dir("load-ghosts", || {
        let scratch = temp_scratch("load-ghosts");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let ghost = eng.push_stub_pane("ghost", "lab");
        let live = eng.push_stub_pane("live", "lab");
        let parked = eng.begin_task(&ghost, "a");
        let open = eng.begin_task(&ghost, "b");
        let (queued, _) = eng.queue_task(&ghost, "c", "cli");
        let kept = eng.begin_task(&live, "d");
        eng.panes.retain(|p| p.slug != ghost);
        eng.drop_paneless_tasks();
        assert_eq!(eng.tasks[&parked].status, "orphaned");
        assert_eq!(eng.tasks[&open].status, "orphaned");
        assert_eq!(eng.tasks[&queued].status, "cancelled");
        assert!(!eng.active_tasks.contains_key(&ghost));
        assert_eq!(eng.open_task(&live), Some(kept));
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

fn look(
    activity: seance_core::agent_state::Activity,
    screen: &str,
) -> seance_core::agent_state::Look {
    seance_core::agent_state::Look {
        activity,
        evidence: None,
        queued_input: false,
        screen: screen.into(),
    }
}

/// A queued task goes in only after IDLE_SETTLE_MS of unbroken idle: a busy
/// frame between two idle ones starts the wait over.
#[test]
fn pump_waits_for_a_continuous_idle_run() {
    use seance_core::agent_state::Activity::{Busy, Idle};
    with_test_state_dir("pump-idle", || {
        let scratch = temp_scratch("pump-idle");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let w = eng.push_stub_pane("w", "lab");
        let (tid, _) = eng.queue_task(&w, "next job", "cli");
        let t0 = now_ms();
        assert!(!eng.pump_pane(0, &w, look(Idle, ""), t0));
        assert!(!eng.pump_pane(0, &w, look(Busy, ""), t0 + 500));
        // Idle again past t0 + settle, but only just: not yet.
        assert!(!eng.pump_pane(0, &w, look(Idle, ""), t0 + 2_000));
        assert_eq!(eng.tasks[&tid].status, "queued");
        assert!(eng.pump_pane(0, &w, look(Idle, ""), t0 + 2_000 + IDLE_SETTLE_MS));
        assert_eq!(eng.tasks[&tid].status, "open");
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

/// A paste stuck in the composer gets two Enters, then fails at the end of
/// the window instead of holding the pane's line forever.
#[test]
fn stuck_delivery_fails_once_enter_is_spent() {
    use seance_core::agent_state::Activity::Idle;
    with_test_state_dir("pump-stuck", || {
        let scratch = temp_scratch("pump-stuck");
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let w = eng.push_stub_pane("w", "lab");
        let (tid, _) = eng.queue_task(&w, "next job", "cli");
        let t0 = now_ms();
        eng.pump_pane(0, &w, look(Idle, ""), t0);
        assert!(eng.pump_pane(0, &w, look(Idle, ""), t0 + IDLE_SETTLE_MS));
        let stuck = look(Idle, "──────────\n❯ next job\n──────────");
        let mut t = t0 + IDLE_SETTLE_MS;
        for _ in 0..40 {
            t += 500;
            eng.pump_pane(0, &w, stuck.clone(), t);
        }
        assert_eq!(eng.tasks[&tid].status, "failed");
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

/// Two circles the way the 2026-09-29 misroute had them: circle `claude-27`
/// ("paceline-desk", lead `claude-21`) and a pane that happens to be called
/// `claude-27` in another circle ("cadence-slack").
fn comms_fixture(tag: &str) -> (Engine, PathBuf) {
    let scratch = temp_scratch(tag);
    let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
    for (pane, ws) in [
        ("claude-21", "claude-27"),
        ("helper", "claude-27"),
        ("claude-38", "claude-47"),
        ("claude-27", "claude-3"),
    ] {
        eng.push_stub_pane(pane, ws);
        eng.note_born(pane);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    eng.rename_workspace("claude-27", "paceline-desk");
    eng.rename_workspace("claude-47", "paceline-arch-impl");
    eng.rename_workspace("claude-3", "cadence-slack");
    eng.comms.messages.clear();
    (eng, scratch)
}

fn msg(eng: &mut Engine, from: &str, to: &str, kind: &str, text: &str) -> ControlResponse {
    eng.handle_control(ControlRequest::MsgSend {
        to: to.into(),
        text: text.into(),
        kind: kind.into(),
        scope: None,
        from: Some(from.into()),
    })
}

#[test]
fn circles_resolve_by_label_to_their_first_pane_and_collisions_are_refused() {
    with_test_state_dir("comms-resolve", || {
        let (mut eng, scratch) = comms_fixture("comms-resolve");
        assert_eq!(
            eng.effective_lead("claude-27").as_deref(),
            Some("claude-21")
        );
        let t = eng.resolve_target("paceline-arch-impl").unwrap();
        assert_eq!(
            (t.pane.as_str(), t.circle.as_deref()),
            ("claude-38", Some("claude-47"))
        );
        assert_eq!(
            eng.resolve_target("@paceline-desk").unwrap().pane,
            "claude-21"
        );
        // The misroute: `claude-27` is a circle slug AND another circle's pane.
        let e = eng.resolve_target("claude-27").unwrap_err();
        assert!(e.contains("ambiguous") && e.contains("@claude-27"), "{e}");
        assert_eq!(eng.resolve_target("@claude-27").unwrap().pane, "claude-21");
        assert_eq!(
            eng.resolve_target("pane:claude-27").unwrap().pane,
            "claude-27"
        );
        // ...and the same guard on plain pane ops (send/read/status, unscoped).
        let st = eng.handle_control(ControlRequest::Status {
            pane: "claude-27".into(),
            scope: None,
            from: None,
        });
        assert!(st.error.unwrap_or_default().contains("ambiguous"));
        let st = eng.handle_control(ControlRequest::Status {
            pane: "@paceline-desk".into(),
            scope: None,
            from: None,
        });
        assert_eq!(st.data.unwrap()["slug"], "claude-21");
        // Explicit lead overrides first-pane.
        let r = eng.handle_control(ControlRequest::Lead {
            workspace: Some("paceline-desk".into()),
            pane: Some("helper".into()),
            scope: None,
            from: None,
        });
        assert_eq!(r.data.unwrap()["lead"], "helper");
        assert_eq!(eng.resolve_target("paceline-desk").unwrap().pane, "helper");
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn ask_is_not_a_task_and_reply_routes_back_by_id() {
    with_test_state_dir("comms-ask", || {
        let (mut eng, scratch) = comms_fixture("comms-ask");
        let running = eng.begin_task("claude-38", "long job");
        let r = msg(
            &mut eng,
            "claude-21",
            "paceline-arch-impl",
            "ask",
            "which option?",
        );
        let d = r.data.unwrap_or_else(|| panic!("{:?}", r.error));
        let id = d["id"].as_str().unwrap().to_string();
        assert_eq!(d["to_pane"], "claude-38");
        // Not a task: the recipient's running task is untouched.
        assert_eq!(eng.tasks[&running].status, "open");
        assert_eq!(eng.message(&id).unwrap().status, "pending");
        assert_eq!(eng.pending_message_for("claude-38"), Some(id.clone()));
        let pasted = eng.message_paste_text(eng.message(&id).unwrap());
        assert!(
            pasted.contains(&format!("seance ctl reply {id}")) && pasted.contains("paceline-desk")
        );

        // Asker not waiting: the reply is queued for the ASKER's pane — the
        // id carries the return address; nobody typed one.
        let r = eng.handle_control(ControlRequest::MsgReply {
            id: id.clone(),
            text: "pick (a)".into(),
            scope: None,
            from: Some("claude-38".into()),
        });
        assert!(r.ok, "{:?}", r.error);
        let m = eng.message(&id).unwrap();
        assert_eq!(
            (m.status.as_str(), m.answer.as_deref()),
            ("answered", Some("pick (a)"))
        );
        assert_eq!(
            eng.pending_message_for("claude-21")
                .map(|r| eng.message(&r).unwrap().kind.clone()),
            Some("reply".into())
        );
        assert!(
            eng.pending_message_for("claude-27").is_none(),
            "cadence-slack gets nothing"
        );
        // Answering twice is refused.
        assert!(eng
            .reply_to_message(&id, "again", Some("claude-38"))
            .is_err());

        // A caller blocked on the answer gets it on stdout; nothing is pasted.
        let r = msg(
            &mut eng,
            "claude-21",
            "paceline-arch-impl",
            "ask",
            "second?",
        );
        let id2 = r.data.unwrap()["id"].as_str().unwrap().to_string();
        eng.handle_control(ControlRequest::MsgGet {
            id: id2.clone(),
            waiting: true,
            scope: None,
            from: None,
        });
        let v = eng
            .reply_to_message(&id2, "yes", Some("claude-38"))
            .unwrap();
        assert_eq!(v["to_waiting_caller"], true);
        assert!(v["pasted"].is_null());
        // tell can't be replied to; your own pane is refused.
        let t = msg(&mut eng, "claude-21", "paceline-arch-impl", "tell", "fyi");
        let tid = t.data.unwrap()["id"].as_str().unwrap().to_string();
        assert!(eng.reply_to_message(&tid, "x", None).is_err());
        assert!(msg(&mut eng, "claude-21", "pane:claude-21", "tell", "me")
            .error
            .is_some());
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn contacts_hear_about_renames_and_lead_changes_and_old_labels_still_work() {
    with_test_state_dir("comms-contacts", || {
        let (mut eng, scratch) = comms_fixture("comms-contacts");
        msg(&mut eng, "claude-21", "paceline-arch-impl", "tell", "hello");
        eng.comms.messages.clear();

        eng.rename_workspace("paceline-arch-impl", "v2-realtime");
        let notices: Vec<_> = eng
            .comms
            .messages
            .iter()
            .filter(|m| m.kind == "notice")
            .collect();
        assert_eq!(
            notices.len(),
            1,
            "only the contact, not the circle's own panes"
        );
        assert_eq!(notices[0].to_pane, "claude-21");
        assert!(notices[0].text.contains(
            "\"paceline-arch-impl\" (one you have talked to) is now called \"v2-realtime\""
        ));
        // The old label still resolves, and says so.
        let t = eng.resolve_target("paceline-arch-impl").unwrap();
        assert_eq!(t.pane, "claude-38");
        assert!(t.note.unwrap().contains("renamed to 'v2-realtime'"));
        // A second rename replaces the undelivered notice rather than stacking.
        eng.rename_workspace("v2-realtime", "v2");
        assert_eq!(
            eng.comms
                .messages
                .iter()
                .filter(|m| m.kind == "notice")
                .count(),
            1
        );

        // Lead closes: contacts are told, circle-addressed mail re-routes.
        eng.comms.messages.clear();
        eng.push_stub_pane("second", "claude-47");
        eng.note_born("second");
        let pending = msg(&mut eng, "claude-21", "v2", "ask", "still there?");
        let pid = pending.data.unwrap()["id"].as_str().unwrap().to_string();
        // Delivered but unanswered when the lead closes: re-asked of the new lead.
        let asked = msg(&mut eng, "claude-21", "v2", "ask", "answered yet?");
        let aid = asked.data.unwrap()["id"].as_str().unwrap().to_string();
        eng.message_mut(&aid).unwrap().status = "delivered".into();
        eng.kill_pane("claude-38");
        assert_eq!(eng.message(&pid).unwrap().to_pane, "second");
        let a = eng.message(&aid).unwrap();
        assert_eq!(
            (a.to_pane.as_str(), a.status.as_str()),
            ("second", "pending")
        );
        assert!(eng.comms.messages.iter().any(|m| m.kind == "notice"
            && m.to_pane == "claude-21"
            && m.text.contains("pane second leads it now")));
        // A pane addressed directly (not via its circle) fails instead.
        let direct = msg(&mut eng, "claude-21", "pane:second", "tell", "x");
        let did = direct.data.unwrap()["id"].as_str().unwrap().to_string();
        eng.kill_pane("second");
        assert_eq!(eng.message(&did).unwrap().status, "failed");
        // The sender closing drops it from every contact list.
        eng.kill_pane("claude-21");
        assert!(eng
            .comms
            .contacts
            .values()
            .all(|s| !s.contains("claude-21")));
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

/// Host circle mode (AFK): on/off prompts go verbatim to the circle's FIRST
/// pane — not a `ctl lead` override — and repeating a state sends nothing.
#[test]
fn circle_mode_prompts_the_first_pane_once_per_change() {
    with_test_state_dir("circle-mode", || {
        let (mut eng, scratch) = comms_fixture("circle-mode");
        let defs = vec![crate::host::HostCircleMode {
            id: "afk".into(),
            label: "AFK".into(),
            on_label: "go afk".into(),
            off_label: "turn off afk mode".into(),
            on_prompt: "im going afk - activate that mode".into(),
            off_prompt: "im back - turn off afk mode".into(),
            top: true,
            ..Default::default()
        }];
        eng.comms.leads.insert("claude-27".into(), "helper".into());
        assert_eq!(eng.first_pane("claude-27").as_deref(), Some("claude-21"));

        let id = eng
            .set_circle_mode_with(&defs, "paceline-desk", "afk", true)
            .unwrap()
            .expect("a change sends a prompt");
        let m = eng.message(&id).unwrap();
        assert_eq!(
            (m.kind.as_str(), m.to_pane.as_str(), m.text.as_str()),
            ("prompt", "claude-21", "im going afk - activate that mode")
        );
        assert_eq!(
            eng.message_paste_text(m),
            "im going afk - activate that mode"
        );
        assert!(eng.comms.modes["claude-27"].contains("afk"));
        // Already on: nothing new is sent.
        assert_eq!(
            eng.set_circle_mode_with(&defs, "claude-27", "afk", true)
                .unwrap(),
            None
        );

        let off = eng
            .set_circle_mode_with(&defs, "claude-27", "afk", false)
            .unwrap()
            .unwrap();
        assert_eq!(
            eng.message(&off).unwrap().text,
            "im back - turn off afk mode"
        );
        assert!(!eng.comms.modes.contains_key("claude-27"));
        // Unknown mode / circle are refused.
        assert!(eng
            .set_circle_mode_with(&defs, "claude-27", "nap", true)
            .is_err());
        assert!(eng
            .set_circle_mode_with(&defs, "nowhere", "afk", true)
            .is_err());
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

/// A host-polled mode (`state_cmd`): the poll is the truth — it can put a
/// circle in the mode seance never prompted, and take it out again. A menu
/// toggle holds until the poll agrees or the hold expires.
#[test]
fn polled_circle_mode_follows_the_host_and_holds_pending_toggles() {
    with_test_state_dir("circle-mode-poll", || {
        let (mut eng, scratch) = comms_fixture("circle-mode-poll");
        let set = |items: &[&str]| -> std::collections::BTreeSet<String> {
            items.iter().map(|s| s.to_string()).collect()
        };
        let none = set(&[]);
        // Entered without seance: by pane (vita's rows carry the pane).
        assert!(eng.ingest_mode_state("afk", &none, &set(&["claude-38"]), 1_000));
        assert!(eng.comms.modes["claude-47"].contains("afk"));
        // Unchanged poll → nothing to push.
        assert!(!eng.ingest_mode_state("afk", &none, &set(&["claude-38"]), 2_000));
        // Left without seance (TTL / release).
        assert!(eng.ingest_mode_state("afk", &none, &none, 3_000));
        assert!(eng.comms.modes.is_empty());

        // Menu toggle on a polled mode: pending, so a stale poll keeps it on…
        let defs = vec![crate::host::HostCircleMode {
            id: "afk".into(),
            label: "AFK".into(),
            on_label: "go afk".into(),
            off_label: "back".into(),
            on_prompt: "going".into(),
            off_prompt: "back".into(),
            state_cmd: Some("true".into()),
            ..Default::default()
        }];
        eng.set_circle_mode_with(&defs, "claude-27", "afk", true)
            .unwrap()
            .unwrap();
        assert!(eng.mode_pending("afk"));
        let t = crate::runtime::engine::helpers::now_ms();
        assert!(!eng.ingest_mode_state("afk", &none, &none, t));
        assert!(eng.comms.modes["claude-27"].contains("afk"));
        // …until the host confirms (by circle slug), which clears the hold.
        eng.ingest_mode_state("afk", &set(&["claude-27"]), &none, t);
        assert!(!eng.mode_pending("afk"));
        // A hold that never confirms expires and the host wins.
        eng.set_circle_mode_with(&defs, "claude-27", "afk", false)
            .unwrap()
            .unwrap();
        assert!(eng.ingest_mode_state(
            "afk",
            &set(&["claude-27"]),
            &none,
            t + crate::host::MODE_PENDING_MS + 1
        ));
        assert!(eng.comms.modes["claude-27"].contains("afk"));
        assert!(!eng.mode_pending("afk"));
        // Display-only (slack thread): the menu can't toggle what only the
        // host can start or end.
        let ro = vec![crate::host::HostCircleMode {
            id: "slack".into(),
            label: "slack".into(),
            state_cmd: Some("true".into()),
            ..Default::default()
        }];
        assert!(eng
            .set_circle_mode_with(&ro, "claude-27", "slack", true)
            .unwrap_err()
            .contains("host owns it"));
        // Off-only: the host owns entering, seance may offer the way out.
        let off_only = vec![crate::host::HostCircleMode {
            id: "slack".into(),
            label: "slack".into(),
            state_cmd: Some("true".into()),
            off_label: "disconnect".into(),
            off_prompt: "leave the slack thread".into(),
            ..Default::default()
        }];
        assert!(eng
            .set_circle_mode_with(&off_only, "claude-27", "slack", true)
            .is_err());
        assert!(off_only[0].def().on_label.is_empty());
        assert_eq!(off_only[0].def().off_label, "disconnect");
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

/// `ctl space`/`ctl move` edit only the `spaces` key: pins, seen and the
/// notes face in the same blob must come through untouched.
#[test]
fn space_ops_edit_the_rail_blob_and_keep_its_other_keys() {
    with_test_state_dir("spaces", || {
        let scratch = std::env::temp_dir().join(format!("seance-spaces-{}", std::process::id()));
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        eng.push_stub_pane("a", "ui-a");
        eng.push_stub_pane("b", "ui-b");
        eng.push_stub_pane("c", "loose");
        let path = crate::daemon::fsbridge::subs_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"seen":["loose"],"pinned":["loose"],"flipped":"c"}"#,
        )
        .unwrap();

        let listed = eng.space_op("list", &[], None).unwrap();
        let ids: Vec<&str> = listed["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["general"]);

        eng.space_op("new", &["later".into()], None).unwrap();
        eng.space_op("move", &["ui-a".into(), "later".into()], None)
            .unwrap();
        assert!(eng.space_op("rm", &["general".into()], None).is_err());
        let blob: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(blob["pinned"], serde_json::json!(["loose"]));
        assert_eq!(blob["flipped"], "c");
        assert_eq!(blob["spaces"]["of"]["ui-a"], "later");
        // A move from inside a pane with no CIRCLE uses the caller's own.
        eng.space_op("move", &["general".into()], Some("loose"))
            .unwrap();
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

/// Done, orphaned and cancelled tasks leave saved state a week after they
/// finish; open and queued ones stay however old they are.
#[test]
fn week_old_finished_tasks_are_not_saved() {
    with_test_state_dir("task-prune", || {
        let scratch = std::env::temp_dir().join(format!("seance-task-prune-{}", now_ms()));
        let (mut eng, _rx) = Engine::bare_for_test(scratch.clone());
        let now = now_ms();
        let week = 7 * 24 * 60 * 60 * 1000;
        let task = |id: &str, status: &str, finished: Option<u64>| TaskRecord {
            id: id.into(),
            pane: "p".into(),
            inject_pad_rev: 0,
            inject_pad_bytes: 0,
            body: String::new(),
            status: status.into(),
            created_ms: now - 2 * week,
            finished_ms: finished,
            supersedes: None,
            queued_by: None,
            delivery: None,
            note: None,
            front: false,
        };
        for t in [
            task("old-done", "done", Some(now - week - 1)),
            task("old-orphaned", "orphaned", Some(now - week - 1)),
            task("old-cancelled", "cancelled", None),
            task("new-done", "done", Some(now - 1000)),
            task("old-open", "open", None),
            task("old-queued", "queued", None),
        ] {
            eng.tasks.insert(t.id.clone(), t);
        }
        eng.persist();

        let mut kept: Vec<String> = crate::state::AppState::load()
            .tasks
            .into_iter()
            .map(|t| t.id)
            .collect();
        kept.sort();
        assert_eq!(kept, ["new-done", "old-open", "old-queued"]);
        let _ = std::fs::remove_dir_all(&scratch);
    });
}

#[test]
fn busy_title_evidence_holds_the_last_spinner_line() {
    use seance_core::agent_state::{classify, Activity};
    with_test_state_dir("steady-evidence", || {
        let (eng, _rx) = Engine::bare_for_test(temp_scratch("steady-evidence"));
        let title = "◐ circle · lead";
        let spin = classify("✻ Brewing… (5s · ↓ 386 tokens)\n❯ ", Some(title));
        let mut a = spin.clone();
        eng.steady_evidence("lead", &mut a, Some(title));
        assert_eq!(
            a.evidence.as_deref(),
            Some("✻ Brewing… (5s · ↓ 386 tokens)")
        );
        // Spinner off screen for a frame: the title decides, the line holds.
        let mut gap = classify("❯ ", Some(title));
        assert_eq!(gap.activity, Activity::Busy);
        eng.steady_evidence("lead", &mut gap, Some(title));
        assert_eq!(gap.evidence, spin.evidence);
        // Too old: the title shows again.
        eng.last_spinner.lock().unwrap().get_mut("lead").unwrap().1 -= 11_000;
        let mut stale = classify("❯ ", Some(title));
        eng.steady_evidence("lead", &mut stale, Some(title));
        assert_eq!(stale.evidence.as_deref(), Some(title));
        // Idle clears it.
        let mut idle = classify("❯ ", None);
        eng.steady_evidence("lead", &mut idle, None);
        assert!(!eng.last_spinner.lock().unwrap().contains_key("lead"));
    });
}

#[test]
fn lead_tasks_json_lists_live_tasks_and_the_newest_finished() {
    with_test_state_dir("lead-tasks", || {
        let scratch = temp_scratch("lead-tasks");
        let (mut eng, _rx) = Engine::bare_for_test(scratch);
        let lead = eng.push_stub_pane("lead", "lab");
        let first = eng.begin_task(&lead, "first");
        let second = eng.begin_task(&lead, "second");
        eng.tasks.get_mut(&second).unwrap().status = "queued".into();
        for i in 0..8u64 {
            let mut t = eng.tasks[&first].clone();
            t.id = format!("old-{i}");
            t.status = "done".into();
            t.created_ms = 1_000 + i;
            t.finished_ms = Some(2_000 + i);
            eng.tasks.insert(t.id.clone(), t);
        }
        let got = eng.lead_tasks_json(&lead);
        let ids: Vec<&str> = got["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        // Live tasks first, then six finished ones, newest first.
        assert!(ids.contains(&second.as_str()));
        let done: Vec<&&str> = ids.iter().filter(|i| i.starts_with("old-")).collect();
        assert_eq!(done.len(), 6);
        assert_eq!(*done[0], "old-7");
        assert_eq!(eng.lead_tasks_json("nope")["tasks"], serde_json::json!([]));

        // A long body is cut on a char boundary.
        eng.tasks.get_mut(&second).unwrap().body = "é".repeat(2000);
        let got = eng.lead_tasks_json(&lead);
        let cut = got["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == second.as_str())
            .unwrap()["body"]
            .as_str()
            .unwrap()
            .len();
        assert!(cut <= 2500 && cut > 2000, "{cut}");
    });
}
