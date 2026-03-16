use std::path::Path;

/// Spawn a fully detached subprocess that survives parent exit and terminal close.
///
/// On Unix this uses the double-fork pattern (fork → setsid → close_fd → fork → exec)
/// so the child is reparented to init and cannot receive SIGHUP from the terminal.
///
/// `args` is the full argv (program + arguments).
/// `env` is a list of (key, value) pairs to set in the child environment.
pub fn spawn_detached(args: &[&str], env: &[(&str, &str)]) {
    if args.is_empty() {
        return;
    }

    #[cfg(unix)]
    {
        spawn_detached_unix(args, env);
    }

    #[cfg(windows)]
    {
        spawn_detached_windows(args, env);
    }
}

#[cfg(unix)]
fn spawn_detached_unix(args: &[&str], env: &[(&str, &str)]) {
    use fork::{fork, setsid, Fork};

    // First fork — parent returns immediately.
    match fork() {
        Ok(Fork::Parent(_)) => {}
        Ok(Fork::Child) => {
            // Create a new session so we detach from the controlling terminal.
            let _ = setsid();

            // Second fork — the intermediate child exits so the grandchild
            // is reparented to init/PID 1 and can never reacquire a terminal.
            match fork() {
                Ok(Fork::Parent(_)) => {
                    // Intermediate child exits immediately.
                    std::process::exit(0);
                }
                Ok(Fork::Child) => {
                    // Close stdin/stdout/stderr so the grandchild doesn't hold
                    // any references to the original terminal.
                    let _ = fork::close_fd();

                    // Set environment variables before exec.
                    for (key, value) in env {
                        std::env::set_var(key, value);
                    }

                    // Replace the process with the target command.
                    let err = exec::execvp(args[0], args);
                    // If execvp returns, it failed.
                    eprintln!("spawn_detached: exec failed: {err}");
                    std::process::exit(1);
                }
                Err(_) => std::process::exit(1),
            }
        }
        Err(_) => {
            tracing::debug!("spawn_detached: first fork failed");
        }
    }
}

#[cfg(windows)]
fn spawn_detached_windows(args: &[&str], env: &[(&str, &str)]) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;

    let mut cmd = std::process::Command::new(args[0]);
    if args.len() > 1 {
        cmd.args(&args[1..]);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS);

    if let Err(err) = cmd.spawn() {
        tracing::debug!(%err, "spawn_detached: failed to spawn on Windows");
    }
}

/// Write JSON bytes to a temp file in `~/.fabro/tmp/` and return the path.
/// Returns `None` if the home directory is unavailable or write fails.
pub fn write_temp_json(filename: &str, json: &[u8]) -> Option<std::path::PathBuf> {
    let tmp_dir = dirs::home_dir()?.join(".fabro").join("tmp");
    std::fs::create_dir_all(&tmp_dir).ok()?;
    let path = tmp_dir.join(filename);
    std::fs::write(&path, json).ok()?;
    Some(path)
}

/// Build the argv for spawning a `fabro` hidden subcommand with a temp file path.
pub fn build_fabro_argv<'a>(exe: &'a str, subcommand: &'a str, path: &'a str) -> Vec<&'a str> {
    vec![exe, subcommand, path]
}

/// Convenience: get the current exe as a String, or None.
pub fn current_exe_str() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
}

/// Check whether a path exists (for tests / cleanup verification).
pub fn path_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_temp_json_creates_file() {
        let json = br#"{"test": true}"#;
        let path = write_temp_json("fabro-test-spawn.json", json).expect("write_temp_json failed");
        assert!(path.exists());
        let contents = std::fs::read(&path).unwrap();
        assert_eq!(contents, json);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn build_fabro_argv_correct() {
        let argv = build_fabro_argv("/usr/bin/fabro", "__send_analytics", "/tmp/event.json");
        assert_eq!(
            argv,
            vec!["/usr/bin/fabro", "__send_analytics", "/tmp/event.json"]
        );
    }

    #[test]
    fn current_exe_str_returns_some() {
        // In test context there is always a current exe.
        assert!(current_exe_str().is_some());
    }

    #[test]
    fn spawn_detached_empty_args_is_noop() {
        // Should not panic or do anything.
        spawn_detached(&[], &[]);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_detached_unix_creates_marker_file() {
        // Spawn a detached `touch <marker>` and verify the file appears.
        let tmp = std::env::temp_dir().join("fabro-spawn-detached-test-marker");
        let _ = std::fs::remove_file(&tmp);

        let tmp_str = tmp.to_str().unwrap();
        spawn_detached(&["touch", tmp_str], &[]);

        // Wait a bit for the detached process to complete.
        std::thread::sleep(std::time::Duration::from_millis(500));

        assert!(
            tmp.exists(),
            "detached process should have created the marker file"
        );
        std::fs::remove_file(&tmp).ok();
    }
}
