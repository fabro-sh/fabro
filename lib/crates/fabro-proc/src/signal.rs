/// Check whether a process with the given PID currently exists.
///
/// On Unix, sends signal 0 via `kill(2)`. Returns `false` if the pid does not
/// fit in `i32`. On non-Unix platforms, conservatively returns `true`.
pub fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: kill(pid, 0) is a read-only probe; it does not deliver a signal.
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Check whether a process with the given PID is still running.
///
/// On Unix, delegates to `process_exists` (a `kill(pid, 0)` probe). Unreaped
/// zombies count as running here because the callers are hot paths (test
/// harness marker scans, daemon-liveness probes) where a zombie window is
/// sub-millisecond and the ~2 ms cost of an authoritative zombie check via
/// `ps` would dominate. For the narrow set of callers that genuinely need
/// to treat zombies as stopped (notably the `fabro server stop` polling
/// loop, which can wait out its full timeout on a zombie child), use
/// `process_running_strict`.
pub fn process_running(pid: u32) -> bool {
    process_exists(pid)
}

/// Like `process_running`, but treats zombie / defunct processes as not
/// running.
///
/// On Unix, follows a cheap `kill(pid, 0)` probe with a `ps` shell-out to
/// read the process state character and excludes `Z`/`z` entries. Falls
/// back to `process_exists(pid)` when the `ps` probe fails, preserving the
/// old conservative behavior. On non-unix, identical to `process_exists`.
///
/// Prefer `process_running` unless you are polling for a child you cannot
/// `wait()` on — the `ps` invocation costs ~2 ms per call on macOS and is
/// wasted on hot paths that have no zombie exposure.
pub fn process_running_strict(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if !process_exists(pid) {
            return false;
        }
        unix_process_state(pid).is_none_or(|state| !matches!(state, 'Z' | 'z'))
    }
    #[cfg(not(unix))]
    {
        process_exists(pid)
    }
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "Unix process-state detection shells out to ps to distinguish running processes from zombies"
)]
fn unix_process_state(pid: u32) -> Option<char> {
    let output = std::process::Command::new("ps")
        .args(["-ww", "-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .chars()
        .find(|ch| !ch.is_whitespace())
}

/// Check whether any process in the given process group is alive.
///
/// On Unix, sends signal 0 to `-pgid` via `kill(2)`. Returns `false` if the
/// process-group id does not fit in `i32`. On non-Unix platforms,
/// conservatively returns `true`.
pub fn process_group_alive(pgid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pgid_i32) = i32::try_from(pgid) else {
            return false;
        };
        // SAFETY: kill(-pgid, 0) is a read-only probe for the process group.
        if unsafe { libc::kill(-pgid_i32, 0) } != 0 {
            return false;
        }
        // Linux's kill(-pgid, 0) succeeds even when every member is a zombie
        // waiting to be reaped, unlike macOS. Disambiguate so callers polling
        // on group liveness don't burn their grace period on a dead group.
        #[cfg(target_os = "linux")]
        {
            linux_group_has_non_zombie(pgid)
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        true
    }
}

#[cfg(target_os = "linux")]
fn linux_group_has_non_zombie(pgid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // Format: "pid (comm) state ppid pgrp ..."; comm may contain ')' so
        // anchor on the final ')' before parsing subsequent fields.
        let Some((_, tail)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = tail.split_whitespace();
        let state = fields.next();
        let _ppid = fields.next();
        let Some(pgrp) = fields.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pgrp == pgid && state != Some("Z") {
            return true;
        }
    }
    false
}

/// Send SIGTERM to a single process.
#[cfg(unix)]
pub fn sigterm(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill with a valid pid and SIGTERM is safe.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

/// Send SIGKILL to a single process.
#[cfg(unix)]
pub fn sigkill(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill with a valid pid and SIGKILL is safe.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Send SIGTERM to an entire process group.
#[cfg(unix)]
pub fn sigterm_process_group(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill with -pid signals the process group.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
}

/// Send SIGKILL to an entire process group.
#[cfg(unix)]
pub fn sigkill_process_group(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill with -pid signals the process group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Send SIGUSR1 to a single process.
#[cfg(unix)]
pub fn sigusr1(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill with a valid pid and SIGUSR1 is safe.
        unsafe {
            libc::kill(pid, libc::SIGUSR1);
        }
    }
}

/// Send SIGUSR2 to a single process.
#[cfg(unix)]
pub fn sigusr2(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill with a valid pid and SIGUSR2 is safe.
        unsafe {
            libc::kill(pid, libc::SIGUSR2);
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_types,
    reason = "Tests use sync std::io::BufReader to read a short-lived helper's stdout synchronously."
)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::{process_exists, process_group_alive, process_running, process_running_strict};
    use crate::pre_exec::pre_exec_setpgid;

    #[test]
    fn process_running_returns_true_for_current_process() {
        assert!(process_exists(std::process::id()));
        assert!(process_running(std::process::id()));
        assert!(process_running_strict(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "zombie-detection test needs to spawn a short-lived child and intentionally leave it unreaped"
    )]
    fn process_running_strict_returns_false_for_unreaped_zombie_child() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("short-lived child should spawn");
        let pid = child.id();

        std::thread::sleep(Duration::from_millis(100));

        assert!(
            process_exists(pid),
            "unreaped zombie should still have a visible pid"
        );
        assert!(
            process_running(pid),
            "cheap process_running treats zombies as alive by design"
        );
        assert!(
            !process_running_strict(pid),
            "process_running_strict should treat zombies as stopped"
        );

        let _status = child.wait().expect("child should remain waitable");
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "process-group test spawns a child in its own process group and observes the group probe"
    )]
    fn process_group_alive_returns_true_for_running_process_group() {
        let mut child = Command::new("sh");
        child
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        pre_exec_setpgid(&mut child);
        let mut child = child.spawn().expect("group leader should spawn");
        let pgid = child.id();

        assert!(
            process_group_alive(pgid),
            "running process group should count as alive"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "process-group zombie test uses a short perl helper that forks without reaping its child"
    )]
    fn process_group_alive_returns_false_for_zombie_only_process_group() {
        let mut parent = Command::new("perl");
        parent
            .args([
                "-MPOSIX",
                "-e",
                r#"$|=1; $pid=fork(); die $! unless defined $pid; if(!$pid){ POSIX::setpgid(0,0) or die $!; exit 0 } print "$pid\n"; sleep 5;"#,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut parent = parent.spawn().expect("zombie parent helper should spawn");
        let stdout = parent.stdout.take().expect("helper stdout should be piped");
        let mut lines = BufReader::new(stdout).lines();
        let child_pid = lines
            .next()
            .expect("helper should print child pid")
            .expect("helper child pid should read")
            .parse::<u32>()
            .expect("helper child pid should parse");

        std::thread::sleep(Duration::from_millis(100));

        assert!(
            !process_group_alive(child_pid),
            "zombie-only process group should not count as alive"
        );

        let _ = parent.kill();
        let _ = parent.wait();
    }
}
