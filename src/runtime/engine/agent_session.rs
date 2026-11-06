//! Which agent CLIs a pane can be put back on after its process dies, and how.
//!
//! claude and grok accept an id we mint (`--session-id`), so the pane owns its
//! conversation from birth. codex can't be told one: the daemon learns it after
//! the first prompt from the rollout file the pane's codex holds open
//! ([`discover_codex_sessions`]), and restore relaunches `codex … resume <id>`.
//!
//! The pane's stored `command` is always the base command (no session args);
//! the id lives in `EnginePane::agent_session` and is re-applied at every
//! launch, so a codex `/new` (which moves the pane to another thread) can't be
//! undone by a stale `resume <id>` baked into the command.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Agent {
    Claude,
    Grok,
    Codex,
}

impl Agent {
    pub(crate) fn of(command: &str) -> Option<Agent> {
        let first = command.split_whitespace().next()?;
        match first.rsplit('/').next()? {
            "claude" => Some(Agent::Claude),
            "grok" => Some(Agent::Grok),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    /// Takes `--session-id <uuid>` on a fresh launch.
    pub(crate) fn mints(self) -> bool {
        matches!(self, Agent::Claude | Agent::Grok)
    }
}

/// A random v4 UUID (claude and grok both reject non-UUID `--session-id`).
pub(crate) fn new_session_uuid() -> String {
    use rand::Rng;
    let b: [u8; 16] = rand::thread_rng().gen();
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

pub(super) fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

fn home_dir(env_var: &str, default: &str) -> PathBuf {
    std::env::var_os(env_var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(shellexpand::tilde(default).into_owned()))
}

fn abs_cwd(cwd_raw: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(cwd_raw).into_owned())
}

/// `<claude home>/projects/<cwd with / and . flattened to ->/<id>.jsonl`.
/// Claude's home is `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub(crate) fn claude_transcript(cwd_raw: &str, session: &str) -> PathBuf {
    let encoded: String = abs_cwd(cwd_raw)
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    home_dir("CLAUDE_CONFIG_DIR", "~/.claude")
        .join("projects")
        .join(encoded)
        .join(format!("{session}.jsonl"))
}

/// `~/.grok/sessions/<percent-encoded cwd>/<id>/chat_history.jsonl`. grok
/// writes it at startup, before the first prompt.
fn grok_transcript(cwd_raw: &str, session: &str) -> PathBuf {
    let encoded: String = abs_cwd(cwd_raw)
        .to_string_lossy()
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    home_dir("GROK_HOME", "~/.grok")
        .join("sessions")
        .join(encoded)
        .join(session)
        .join("chat_history.jsonl")
}

fn codex_sessions_dir() -> PathBuf {
    home_dir("CODEX_HOME", "~/.codex").join("sessions")
}

/// `<codex home>/sessions/YYYY/MM/DD/rollout-<stamp>-<id>.jsonl`.
fn codex_rollout(session: &str) -> Option<PathBuf> {
    let suffix = format!("-{session}.jsonl");
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    for y in dirs(&codex_sessions_dir()) {
        for m in dirs(&y) {
            for d in dirs(&m) {
                for f in dirs(&d) {
                    if f.file_name()
                        .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
                    {
                        return Some(f);
                    }
                }
            }
        }
    }
    None
}

/// The conversation is on disk, so a resume will land instead of exiting
/// non-zero (which closes the pane).
pub(crate) fn transcript_exists(agent: Agent, cwd_raw: &str, session: &str) -> bool {
    match agent {
        Agent::Claude => claude_transcript(cwd_raw, session).is_file(),
        Agent::Grok => grok_transcript(cwd_raw, session).is_file(),
        Agent::Codex => codex_rollout(session).is_some(),
    }
}

/// Split a hand-written command into (base, session id). Recognizes
/// `--resume X` / `--session-id X` (claude, grok; grok's `-r` / `-s` too) and
/// `codex … resume <uuid>`. A `--continue` / bare `resume` names no id.
pub(crate) fn split_session(command: &str) -> (String, Option<String>) {
    let Some(agent) = Agent::of(command) else {
        return (command.to_string(), None);
    };
    let toks: Vec<&str> = command.split_whitespace().collect();
    let (start, len) = match agent {
        Agent::Claude | Agent::Grok => {
            let flags: &[&str] = if agent == Agent::Grok {
                &["--resume", "--session-id", "-r", "-s"]
            } else {
                &["--resume", "--session-id", "-r"]
            };
            let Some(i) = toks.iter().position(|t| flags.contains(t)) else {
                return (command.to_string(), None);
            };
            match toks.get(i + 1).filter(|id| !id.starts_with('-')) {
                Some(_) => (i, 2),
                None => return (command.to_string(), None),
            }
        }
        Agent::Codex => {
            let Some(i) = toks.iter().position(|t| *t == "resume") else {
                return (command.to_string(), None);
            };
            let Some(j) = toks[i + 1..].iter().position(|t| looks_like_uuid(t)) else {
                return (command.to_string(), None);
            };
            // `resume`'s own flags stay, as globals: codex accepts them there.
            let id = toks[i + 1 + j].to_string();
            let mut rest: Vec<&str> = toks.clone();
            rest.remove(i + 1 + j);
            rest.remove(i);
            return (rest.join(" "), Some(id));
        }
    };
    let id = toks[start + 1].to_string();
    let mut rest = toks.clone();
    rest.drain(start..start + len);
    (rest.join(" "), Some(id))
}

/// The launch command for a pane that owns `session`.
///
/// Resume only when the conversation is on disk: a minted id that was never
/// written makes `--resume` exit non-zero, so claude and grok re-assert it
/// with `--session-id` instead, and codex just starts fresh.
pub(crate) fn launch_with_session(command: &str, cwd_raw: &str, session: &str) -> String {
    let Some(agent) = Agent::of(command) else {
        return command.to_string();
    };
    let on_disk = transcript_exists(agent, cwd_raw, session);
    match agent {
        Agent::Claude | Agent::Grok if on_disk => format!("{command} --resume {session}"),
        Agent::Claude | Agent::Grok => format!("{command} --session-id {session}"),
        Agent::Codex if on_disk => format!("{command} resume {session}"),
        Agent::Codex => command.to_string(),
    }
}

/// Rollout path → (thread id, is a subagent thread). Rollouts never change
/// their first line, so this is safe to keep for the daemon's lifetime.
pub(crate) type RolloutCache = HashMap<PathBuf, Option<(String, bool)>>;

fn rollout_meta(path: &Path) -> Option<(String, bool)> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(f).read_line(&mut line).ok()?;
    let v: serde_json::Value = serde_json::from_str(&line).ok()?;
    let p = v.get("payload")?;
    let id = p.get("id")?.as_str()?.to_string();
    let sub = p.get("thread_source").and_then(|s| s.as_str()) == Some("subagent")
        || p.get("parent_thread_id").is_some();
    Some((id, sub))
}

fn proc_children() -> HashMap<u32, Vec<u32>> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return kids;
    };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // comm may hold spaces and parens; ppid is the 2nd field after the last ')'.
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u32>().ok());
        if let Some(ppid) = ppid {
            kids.entry(ppid).or_default().push(pid);
        }
    }
    kids
}

/// For each `(slug, root pid)` of a live codex pane: the main thread its
/// process tree holds open right now. A pane that hasn't been prompted holds
/// none and is left out. Requires codex to run in-pane (`--no-daemon`, which
/// `agents::isolate_codex` adds); a shared app-server holds the files instead.
pub(crate) fn discover_codex_sessions(
    panes: &[(String, u32)],
    cache: &mut RolloutCache,
) -> Vec<(String, String)> {
    if panes.is_empty() {
        return vec![];
    }
    let sessions_dir = codex_sessions_dir();
    let kids = proc_children();
    let mut found = vec![];
    for (slug, root) in panes {
        let mut stack = vec![*root];
        let mut best: Option<(std::time::SystemTime, String)> = None;
        while let Some(pid) = stack.pop() {
            stack.extend(kids.get(&pid).into_iter().flatten().copied());
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                if !target.starts_with(&sessions_dir)
                    || target.extension().is_none_or(|x| x != "jsonl")
                {
                    continue;
                }
                let meta = cache
                    .entry(target.clone())
                    .or_insert_with(|| rollout_meta(&target))
                    .clone();
                let Some((id, false)) = meta else { continue };
                let mtime = std::fs::metadata(&target)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                    best = Some((mtime, id));
                }
            }
        }
        if let Some((_, id)) = best {
            found.push((slug.clone(), id));
        }
    }
    found
}

impl super::Engine {
    /// `(slug, root pid)` of every live codex pane, for [`discover_codex_sessions`].
    pub fn codex_pane_pids(&self) -> Vec<(String, u32)> {
        self.panes
            .iter()
            .filter(|p| Agent::of(&p.command) == Some(Agent::Codex))
            .filter_map(|p| {
                let pid = p.session.as_ref()?.child_pid()?;
                Some((p.slug.clone(), pid))
            })
            .collect()
    }

    /// Record discovered ids. True when anything changed (caller persists).
    pub fn adopt_agent_sessions(&mut self, found: &[(String, String)]) -> bool {
        let mut changed = false;
        for (slug, id) in found {
            if let Some(p) = self.panes.iter_mut().find(|p| &p.slug == slug) {
                if p.agent_session.as_ref() != Some(id) {
                    crate::events::log(
                        "daemon",
                        Some(&p.workspace),
                        Some(slug),
                        "agent_session",
                        format!("codex thread {id}"),
                    );
                    p.agent_session = Some(id.clone());
                    changed = true;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a10927-1280-7b12-9df1-eafba95869c3";

    #[test]
    fn agents_detected_bare_and_by_path() {
        assert_eq!(Agent::of("claude --x"), Some(Agent::Claude));
        assert_eq!(Agent::of("/home/z/.local/bin/claude"), Some(Agent::Claude));
        assert_eq!(Agent::of("grok --yolo"), Some(Agent::Grok));
        assert_eq!(Agent::of("/opt/codex --no-daemon"), Some(Agent::Codex));
        assert_eq!(Agent::of("claude-agent-acp"), None);
        assert_eq!(Agent::of("bash -l"), None);
    }

    #[test]
    fn minted_uuid_is_v4_shaped() {
        let id = new_session_uuid();
        assert!(looks_like_uuid(&id), "{id}");
        assert_ne!(new_session_uuid(), new_session_uuid());
    }

    #[test]
    fn explicit_sessions_split_off_the_base_command() {
        assert_eq!(
            split_session("claude --resume 9f3c1a2b --dangerously-skip-permissions"),
            (
                "claude --dangerously-skip-permissions".into(),
                Some("9f3c1a2b".into())
            )
        );
        assert_eq!(
            split_session("grok --yolo -r abc"),
            ("grok --yolo".into(), Some("abc".into()))
        );
        assert_eq!(
            split_session(&format!("codex --yolo resume {ID}")),
            ("codex --yolo".into(), Some(ID.into()))
        );
        assert_eq!(
            split_session(&format!(
                "codex resume --dangerously-bypass-approvals-and-sandbox --no-daemon {ID}"
            )),
            (
                "codex --dangerously-bypass-approvals-and-sandbox --no-daemon".into(),
                Some(ID.into())
            )
        );
        // codex `-s` is --sandbox, not a session.
        assert_eq!(
            split_session("codex -s danger-full-access"),
            ("codex -s danger-full-access".into(), None)
        );
        assert_eq!(split_session("claude --continue").1, None);
        assert_eq!(split_session("claude --resume").1, None);
        assert_eq!(split_session("codex resume --last").1, None);
        assert_eq!(split_session("bash -l").1, None);
    }

    #[test]
    fn missing_transcript_never_resumes() {
        let cwd = "/tmp/definitely-not-a-project";
        assert_eq!(
            launch_with_session("claude", cwd, "no-such-session"),
            "claude --session-id no-such-session"
        );
        assert_eq!(
            launch_with_session("grok --yolo", cwd, "no-such-session"),
            "grok --yolo --session-id no-such-session"
        );
        assert_eq!(
            launch_with_session("codex --yolo", cwd, "00000000-0000-4000-a000-000000000000"),
            "codex --yolo"
        );
    }

    #[test]
    fn grok_cwd_is_percent_encoded() {
        let p = grok_transcript("/home/you/work/proj", "x");
        assert!(
            p.to_string_lossy()
                .ends_with("sessions/%2Fhome%2Fyou%2Fwork%2Fproj/x/chat_history.jsonl"),
            "{}",
            p.display()
        );
    }

    #[test]
    fn rollout_meta_skips_subagents() {
        let dir = std::env::temp_dir().join(format!("seance-rollout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.jsonl");
        let sub = dir.join("sub.jsonl");
        std::fs::write(
            &main,
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"thread_source\":\"user\"}}}}\n"),
        )
        .unwrap();
        std::fs::write(
            &sub,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"thread_source\":\"subagent\",\"parent_thread_id\":\"p\"}}\n",
        )
        .unwrap();
        assert_eq!(rollout_meta(&main), Some((ID.into(), false)));
        assert_eq!(rollout_meta(&sub), Some(("s".into(), true)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
