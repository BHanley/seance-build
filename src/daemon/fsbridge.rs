//! Daemon-side fs/config bridge: executes [`FsOp`]s for attached GUIs.
//!
//! Thin-client contract: every GUI surface that used to read the local disk —
//! file panes, scratchpads, layout, host widgets — goes through here instead,
//! so a GUI on another machine sees the daemon's world, not its own. Ops run
//! on a per-request thread (spawned in `serve_gui`), so a slow disk or a slow
//! host `select_cmd` never stalls the input pipeline.
//!
//! Also owns the daemon-side host widget poller: one thread polling the
//! configs from this machine's `~/.config/seance/host.json`, broadcasting
//! [`GuiEvent::HostWidgets`] to every attached window.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::host::HostState;
use crate::runtime::protocol::{FsOp, GuiEvent};

use super::SharedEngine;

/// Serializes [`FsOp::WriteIfUnchanged`]: its check and its rename must not
/// interleave with another one (and the lock lets them share a temp name).
static WRITE_IF: Mutex<()> = Mutex::new(());

/// Execute one op. Returns `(ok, data, error)` for the FsResult.
pub fn run(op: FsOp, engine: &SharedEngine) -> (bool, Option<Value>, Option<String>) {
    match run_inner(op, engine) {
        Ok(data) => (true, Some(data), None),
        Err(e) => (false, None, Some(e)),
    }
}

fn run_inner(op: FsOp, engine: &SharedEngine) -> Result<Value, String> {
    match op {
        FsOp::Read { path } => {
            let path = expand(&path);
            let bytes = std::fs::read(&path).map_err(|e| format!("read {path:?}: {e}"))?;
            let mtime = mtime_ms(&path);
            Ok(json!({
                "contents_b64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                "mtime_ms": mtime,
            }))
        }
        FsOp::Write { path, contents_b64 } => {
            let path = expand(&path);
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(contents_b64.as_bytes())
                .map_err(|e| format!("bad base64: {e}"))?;
            atomic_write(&path, &bytes).map_err(|e| format!("write {path:?}: {e}"))?;
            Ok(json!({ "mtime_ms": mtime_ms(&path) }))
        }
        FsOp::WriteIfUnchanged {
            path,
            expected_b64,
            contents_b64,
        } => {
            let b64 = &base64::engine::general_purpose::STANDARD;
            let expected = b64
                .decode(expected_b64.as_bytes())
                .map_err(|e| format!("bad base64: {e}"))?;
            let bytes = b64
                .decode(contents_b64.as_bytes())
                .map_err(|e| format!("bad base64: {e}"))?;
            let real = write_if_unchanged(&expand(&path), &expected, &bytes)?;
            Ok(json!({ "mtime_ms": mtime_ms(&real) }))
        }
        FsOp::Stat { path } => {
            let path = expand(&path);
            match std::fs::metadata(&path) {
                Ok(meta) => Ok(json!({
                    "exists": true,
                    "mtime_ms": meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64),
                    "size": meta.len(),
                })),
                Err(_) => Ok(json!({ "exists": false })),
            }
        }
        FsOp::List { path } => {
            let path = expand(&path);
            let mut entries = Vec::new();
            let rd = std::fs::read_dir(&path).map_err(|e| format!("list {path:?}: {e}"))?;
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push(json!({ "name": name, "is_dir": is_dir }));
            }
            Ok(json!({ "entries": entries }))
        }
        FsOp::Remove { path } => {
            let path = expand(&path);
            std::fs::remove_file(&path).map_err(|e| format!("remove {path:?}: {e}"))?;
            Ok(json!({}))
        }
        FsOp::LayoutLoad => {
            let path = layout_path();
            match std::fs::read_to_string(&path) {
                Ok(s) => Ok(json!({ "json": s })),
                Err(_) => Ok(json!({ "json": null })),
            }
        }
        FsOp::LayoutSave { json: body } => {
            let path = layout_path();
            atomic_write(&path, body.as_bytes()).map_err(|e| format!("layout save: {e}"))?;
            Ok(json!({}))
        }
        FsOp::SubsLoad => {
            let path = subs_path();
            match std::fs::read_to_string(&path) {
                Ok(s) => Ok(json!({ "json": s })),
                Err(_) => Ok(json!({ "json": null })),
            }
        }
        FsOp::SubsSave { json: body } => {
            let path = subs_path();
            atomic_write(&path, body.as_bytes()).map_err(|e| format!("subs save: {e}"))?;
            // Persisting is only half of it: every other window is now
            // rendering a rail that disagrees with the one on disk. Push
            // rather than wait for their next attach — a pin made at the desk
            // should land on the laptop while both are open, which is the
            // whole point of the daemon owning this.
            if let Ok(mut eng) = engine.lock() {
                eng.broadcast(GuiEvent::RailPrefs { json: body });
            }
            Ok(json!({}))
        }
        FsOp::Shell { cmd } => {
            let out = std::process::Command::new("sh")
                .args(["-lc", &cmd])
                .output()
                .map_err(|e| format!("shell: {e}"))?;
            let cap = 64 * 1024;
            let clip = |b: &[u8]| String::from_utf8_lossy(&b[..b.len().min(cap)]).into_owned();
            Ok(json!({
                "status": out.status.code(),
                "stdout": clip(&out.stdout),
                "stderr": clip(&out.stderr),
            }))
        }
        FsOp::Events {
            since_ms,
            since_seq,
            kinds,
            limit,
        } => {
            // A client asks for what it shows; the cap keeps a bad request
            // from shipping the whole log.
            let (events, seq) = crate::events::recent(since_ms, since_seq, &kinds, limit.min(5000));
            Ok(json!({ "events": events, "seq": seq }))
        }
        FsOp::LeadTasks { pane } => {
            let eng = engine.lock().map_err(|_| "engine busy".to_string())?;
            Ok(eng.lead_tasks_json(&pane))
        }
        FsOp::HostSelect { widget, item } => {
            // Fresh load: select is rare and config may have changed on disk.
            let mut host = HostState::load();
            let out = host.select(&widget, &item)?;
            // Push the post-select state to every window right away rather
            // than waiting for the next poll tick.
            host.poll_all();
            let payload = host_payload(&host);
            *LATEST_HOST.lock().unwrap() = Some(payload.clone());
            if let Ok(mut eng) = engine.lock() {
                eng.broadcast(GuiEvent::HostWidgets { widgets: payload });
            }
            Ok(json!({ "output": out }))
        }
    }
}

fn expand(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).as_ref())
}

fn mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

/// Atomic write: temp sibling + rename, dirs created as needed.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("seance-tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// The [`FsOp::WriteIfUnchanged`] write: through a symlink to its target,
/// keeping the file's permissions, and only if it still holds `expected`.
/// Returns the real path written.
fn write_if_unchanged(path: &Path, expected: &[u8], bytes: &[u8]) -> Result<PathBuf, String> {
    let real = std::fs::canonicalize(path).map_err(|e| format!("resolve {path:?}: {e}"))?;
    // One at a time daemon-wide, so two clicks can't both pass the check.
    let _one = WRITE_IF.lock().unwrap_or_else(|p| p.into_inner());
    let current = std::fs::read(&real).map_err(|e| format!("read {real:?}: {e}"))?;
    if current != expected {
        return Err(crate::runtime::protocol::FS_CHANGED.to_string());
    }
    let perms = std::fs::metadata(&real)
        .map_err(|e| format!("stat {real:?}: {e}"))?
        .permissions();
    let name = real.file_name().unwrap_or_default().to_string_lossy();
    let tmp = real.with_file_name(format!(".{name}.seance-{}.tmp", std::process::id()));
    // Born with the file's mode, so new contents are never wider-readable.
    let written = {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let _ = std::fs::remove_file(&tmp); // a crash leftover
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(perms.mode())
            .open(&tmp)
            .and_then(|mut f| f.write_all(bytes))
            .and_then(|_| std::fs::set_permissions(&tmp, perms))
            .and_then(|_| std::fs::rename(&tmp, &real))
    };
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {real:?}: {e}"));
    }
    Ok(real)
}

/// Shared GUI layout lives beside the daemon's state.json.
fn layout_path() -> PathBuf {
    state_dir_file("layout.json")
}

/// The shared rail arrangement, beside the layout. Note this is the daemon's
/// *state* dir, not `~/.config/seance/` where the pre-0.23 per-GUI file lived
/// — a thin client writing the config path would have written its own disk.
pub(crate) fn subs_path() -> PathBuf {
    state_dir_file("subscriptions.json")
}

fn state_dir_file(name: &str) -> PathBuf {
    if let Ok(dir) = std::env::var("SEANCE_STATE_DIR") {
        if !dir.is_empty() {
            if let Ok(expanded) = shellexpand::full(&dir) {
                return PathBuf::from(expanded.as_ref()).join(name);
            }
        }
    }
    PathBuf::from(shellexpand::tilde("~/.local/share/seance/").as_ref()).join(name)
}

// ── daemon-side host widget poller ─────────────────────────────────────────

static LATEST_HOST: Mutex<Option<Value>> = Mutex::new(None);

fn host_payload(host: &HostState) -> Value {
    serde_json::to_value(&host.widgets).unwrap_or(Value::Null)
}

/// Latest host widget snapshot, for pushing to a freshly attached window.
pub fn latest_host_widgets() -> Option<Value> {
    LATEST_HOST.lock().ok().and_then(|g| g.clone())
}

/// Start the poll thread. Broadcasts HostWidgets to all GUI conns each tick.
pub fn start_host_poller(engine: SharedEngine) {
    std::thread::Builder::new()
        .name("seance-host-poll".into())
        .spawn(move || {
            let mut host = HostState::load();
            if !host.enabled() {
                return;
            }
            let interval = std::time::Duration::from_secs(host.min_poll_secs());
            loop {
                host.poll_all();
                let payload = host_payload(&host);
                *LATEST_HOST.lock().unwrap() = Some(payload.clone());
                if let Ok(mut eng) = engine.lock() {
                    eng.broadcast(GuiEvent::HostWidgets { widgets: payload });
                }
                std::thread::sleep(interval);
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// The Today feed's exact request line, parsed the way `serve_gui`
    /// parses it and run by the same handler, reads the daemon's own log.
    #[test]
    fn events_line_runs_through_the_daemon_handler() {
        let _g = crate::state::test_env_lock();
        let prev = std::env::var("SEANCE_STATE_DIR").ok();
        let dir = std::env::temp_dir().join(format!("seance-fs-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SEANCE_STATE_DIR", &dir);
        std::fs::write(
            dir.join("events.jsonl"),
            concat!(
                r#"{"id":"evt_1","seq":1,"ts":5,"actor":"daemon","pane":"a","kind":"pane_exited","detail":"x"}"#,
                "\n",
                r#"{"id":"evt_2","seq":2,"ts":6,"actor":"human","pane":"a","kind":"terminal.input","detail":"k"}"#,
                "\n",
                r#"{"id":"evt_3","seq":3,"ts":7,"actor":"agent:a","workspace":"w","pane":"a","kind":"status_set","detail":"done: ok"}"#,
                "\n",
            ),
        )
        .unwrap();

        let line = r#"{"op":"fs","id":3,"fs_op":"events","since_ms":0,"since_seq":0,"kinds":["status_set","pane_exited"],"limit":50}"#;
        let req: crate::runtime::protocol::GuiRequest = serde_json::from_str(line).unwrap();
        let crate::runtime::protocol::GuiRequest::Fs { id, fs } = req else {
            panic!("not an fs request");
        };
        assert_eq!(id, 3);
        let scratch = dir.join("scratch");
        let (eng, _rx) = crate::runtime::engine::Engine::bare_for_test(scratch);
        let engine: SharedEngine = std::sync::Arc::new(std::sync::Mutex::new(eng));
        let (ok, data, error) = run(fs, &engine);
        assert!(ok, "{error:?}");
        let data = data.unwrap();
        // Tests running beside this one may log real events into the same
        // state dir; ours are the ones stamped in 1970.
        let ours: Vec<&Value> = data["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["ts"].as_u64().is_some_and(|t| t < 100))
            .collect();
        let kinds: Vec<&str> = ours.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["pane_exited", "status_set"]);
        assert_eq!(ours[1]["workspace"], "w");
        // The cursor is the log's last line, not a seq handed out since.
        assert!(data["seq"].as_u64().is_some_and(|s| s >= 3));

        match prev {
            Some(v) => std::env::set_var("SEANCE_STATE_DIR", v),
            None => std::env::remove_var("SEANCE_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_if_unchanged_follows_symlinks_keeps_mode_and_refuses_stale() {
        let dir = std::env::temp_dir().join(format!("seance-wiu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("todo.md");
        std::fs::write(&real, "- [ ] a\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("link.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            write_if_unchanged(&link, b"stale", b"x"),
            Err(crate::runtime::protocol::FS_CHANGED.to_string())
        );
        write_if_unchanged(&link, b"- [ ] a\n", b"- [x] a\n").unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "- [x] a\n");
        let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "temp file left behind"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
