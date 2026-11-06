use std::io;

const SHARED_SERVER_ERROR: &str = "cannot establish this pane's identity: a shared Codex app-server can inherit another pane's SEANCE_SESSION. Relaunch Codex in the intended pane with --no-daemon (or create a new pane with --agent codex). Do not override SEANCE_SESSION to guess.";

pub(super) fn validate(from: Option<&str>) -> Result<(), String> {
    let Some(from) = from else {
        return Ok(());
    };
    validate_ancestry(from, std::process::id(), process, session)
}

fn validate_ancestry(
    from: &str,
    mut pid: u32,
    mut read: impl FnMut(u32) -> io::Result<(u32, Vec<String>)>,
    mut read_session: impl FnMut(u32) -> io::Result<Option<String>>,
) -> Result<(), String> {
    let mut child = pid;
    // Topmost ancestor whose environment carries SEANCE_SESSION: the pane's
    // root process. After a daemon upgrade the pane processes are reparented
    // to a subreaper (`systemd --user` here) whose environ can't be read —
    // 2026-09-29, every pre-upgrade pane's ctl failed with EACCES — so the
    // identity comes from the pane root, not from whatever sits above it.
    // Environments are only read once the whole chain has been checked for a
    // shared Codex server — nothing below it is trusted before that.
    let mut chain: Vec<u32> = Vec::new();
    for _ in 0..128 {
        if pid <= 1 {
            break;
        }
        let (parent, args) = read(pid)
            .map_err(|e| format!("cannot verify pane identity through process {pid}: {e}"))?;
        let executable = args
            .first()
            .and_then(|arg| std::path::Path::new(arg).file_name());
        if executable.is_some_and(|name| name == "codex")
            && args.get(1).is_some_and(|arg| arg == "app-server")
            && args.iter().any(|arg| {
                arg == "--managed-daemon" || arg == "--listen" || arg.starts_with("--listen=")
            })
        {
            return Err(SHARED_SERVER_ERROR.into());
        }
        // A test daemon or a daemon upgrade may itself have agent ancestors.
        if executable.is_some_and(|name| name == "seance")
            && args.get(1).is_some_and(|arg| arg == "daemon")
        {
            return match_session(from, read_session(child));
        }
        chain.push(pid);
        // PTY children survive upgrades, reparented to init (or a subreaper)
        // after the old daemon exits.
        if parent <= 1 {
            let pane_root = chain
                .iter()
                .rev()
                .copied()
                .find(|p| matches!(read_session(*p), Ok(Some(_))));
            return match_session(from, read_session(pane_root.unwrap_or(pid)));
        }
        if parent == pid {
            break;
        }
        child = pid;
        pid = parent;
    }
    Err("cannot verify pane identity: incomplete process ancestry".into())
}

fn match_session(from: &str, original: io::Result<Option<String>>) -> Result<(), String> {
    let original = original.map_err(|e| format!("cannot verify original pane identity: {e}"))?;
    if original.as_deref() != Some(from) {
        return Err(format!("pane identity mismatch: SEANCE_SESSION claims '{from}', but the pane process identifies as '{}'; relaunch the agent in its intended pane", original.as_deref().unwrap_or("unknown")));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process(pid: u32) -> io::Result<(u32, Vec<String>)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let parent = stat
        .rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(1))
        .and_then(|parent| parent.parse().ok())
        .ok_or_else(|| io::Error::other("invalid process stat"))?;
    let args = std::fs::read(format!("/proc/{pid}/cmdline"))?
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    Ok((parent, args))
}

#[cfg(not(target_os = "linux"))]
fn process(pid: u32) -> io::Result<(u32, Vec<String>)> {
    let output = std::process::Command::new("ps")
        .args([
            "-ww",
            "-o",
            "ppid=",
            "-o",
            "command=",
            "-p",
            &pid.to_string(),
        ])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut words = text.split_whitespace();
    let parent = words
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("cannot read process ancestry"))?;
    Ok((parent, words.map(str::to_string).collect()))
}

#[cfg(target_os = "linux")]
fn session(pid: u32) -> io::Result<Option<String>> {
    let environ = std::fs::read(format!("/proc/{pid}/environ"))?;
    Ok(environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"SEANCE_SESSION="))
        .map(|value| String::from_utf8_lossy(value).into_owned()))
}

#[cfg(not(target_os = "linux"))]
fn session(pid: u32) -> io::Result<Option<String>> {
    let output = std::process::Command::new("ps")
        .args(["-Eww", "-o", "command=", "-p", &pid.to_string()])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|word| word.strip_prefix("SEANCE_SESSION="))
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_identity_from_shared_codex_is_rejected() {
        let result = validate_ancestry(
            "worker",
            3,
            |pid| {
                Ok(match pid {
                    3 => (
                        2,
                        vec!["bash".into(), "-lc".into(), "seance ctl finish".into()],
                    ),
                    _ => (
                        1,
                        [
                            "/opt/codex",
                            "app-server",
                            "--listen",
                            "unix://",
                            "--managed-daemon",
                        ]
                        .map(str::to_string)
                        .to_vec(),
                    ),
                })
            },
            |_| panic!("must reject the shared server before trusting env"),
        );
        assert!(result.unwrap_err().contains("--no-daemon"));
    }

    #[test]
    fn local_codex_and_pane_boundary_are_safe() {
        let result = validate_ancestry(
            "worker",
            3,
            |pid| {
                Ok(match pid {
                    3 => (2, vec!["/opt/codex".into(), "--no-daemon".into()]),
                    2 => (99, vec!["/opt/seance".into(), "daemon".into()]),
                    _ => panic!("must stop at the pane's daemon"),
                })
            },
            |pid| {
                assert_eq!(pid, 3);
                Ok(Some("worker".into()))
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn stale_snapshot_does_not_override_original_pane() {
        let result = validate_ancestry(
            "wrong-pane",
            3,
            |pid| {
                Ok(match pid {
                    3 => (2, vec!["codex".into(), "--no-daemon".into()]),
                    _ => (1, vec!["seance".into(), "daemon".into()]),
                })
            },
            |_| Ok(Some("actual-pane".into())),
        );
        assert!(result.unwrap_err().contains("identity mismatch"));
        assert!(
            validate_ancestry("worker", 2, |_| Ok((1, vec!["bash".into()])), |_| Ok(None)).is_err()
        );
    }

    #[test]
    fn daemon_upgrade_reparenting_preserves_identity() {
        assert!(validate_ancestry(
            "worker",
            3,
            |_| Ok((1, vec!["codex".into(), "--no-daemon".into()])),
            |pid| {
                assert_eq!(pid, 3);
                Ok(Some("worker".into()))
            },
        )
        .is_ok());
    }

    #[test]
    fn subreaper_above_the_pane_after_upgrade_is_skipped() {
        // ctl(5) <- claude(4, the pane root) <- systemd --user(3, unreadable) <- init
        let tree = |pid: u32| -> io::Result<(u32, Vec<String>)> {
            Ok(match pid {
                5 => (4, vec!["seance".into(), "ctl".into()]),
                4 => (3, vec!["claude".into()]),
                3 => (1, vec!["/usr/lib/systemd/systemd".into(), "--user".into()]),
                _ => unreachable!(),
            })
        };
        let env = |pid: u32| -> io::Result<Option<String>> {
            match pid {
                3 => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
                _ => Ok(Some("worker".into())),
            }
        };
        assert!(validate_ancestry("worker", 5, tree, env).is_ok());
        // Still checked: the pane root says who the pane is.
        assert!(validate_ancestry("other", 5, tree, env).is_err());
        // No readable identity anywhere still fails closed.
        let none = |pid: u32| -> io::Result<Option<String>> {
            if pid == 3 {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(None)
            }
        };
        assert!(validate_ancestry("worker", 5, tree, none).is_err());
    }

    #[test]
    fn unreadable_identity_fails_closed() {
        assert!(
            validate_ancestry("worker", 3, |_| Err(io::Error::other("gone")), |_| Ok(None))
                .is_err()
        );
    }
}
