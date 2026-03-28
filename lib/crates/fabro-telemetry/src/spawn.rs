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
#[allow(clippy::exit)]
fn spawn_detached_unix(args: &[&str], env: &[(&str, &str)]) {
    use fork::{Fork, fork, setsid};

    // Flush stdout/stderr before forking so the child process doesn't inherit
    // buffered data that would be flushed again on child exit, causing
    // duplicate or corrupted output.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();

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
                    let _err = exec::execvp(args[0], args);
                    // If execvp returns, it failed. stderr is closed so we can't log.
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

/// Serialize data as JSON to a temp file and spawn `fabro <subcommand> <path>`
/// as a fully detached subprocess. Sets `FABRO_TELEMETRY=off` to prevent recursion.
///
/// This is the shared pattern used by both analytics and panic senders.
/// No-ops silently if the exe path can't be resolved or the temp file can't be written.
pub fn spawn_fabro_subcommand(subcommand: &str, filename: &str, json: &[u8]) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let tmp_dir = home.join(".fabro").join("tmp");
    if std::fs::create_dir_all(&tmp_dir).is_err() {
        return;
    }
    let path = tmp_dir.join(filename);
    if std::fs::write(&path, json).is_err() {
        return;
    }

    let Some(path_str) = path.to_str() else {
        return;
    };
    let path_str = path_str.to_string();

    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(std::string::ToString::to_string))
    else {
        return;
    };

    spawn_detached(
        &[&exe, subcommand, &path_str],
        &[("FABRO_TELEMETRY", "off")],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
