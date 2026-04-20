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
/// On Unix, this treats zombie / defunct processes as not running even though
/// they still have a visible PID until their parent reaps them. If the
/// follow-up `ps` probe fails, this falls back to `process_exists(pid)` to
/// preserve the old conservative behavior.
pub fn process_running(pid: u32) -> bool {
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
        let Ok(pgid) = i32::try_from(pgid) else {
            return false;
        };
        // SAFETY: kill(-pgid, 0) is a read-only probe for the process group.
        unsafe { libc::kill(-pgid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        true
    }
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

    use super::{process_exists, process_group_alive, process_running};
    use crate::pre_exec::pre_exec_setpgid;

    #[test]
    fn process_running_returns_true_for_current_process() {
        assert!(process_exists(std::process::id()));
        assert!(process_running(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "process-state test needs to spawn a short-lived child and intentionally leave it unreaped"
    )]
    fn process_running_returns_false_for_unreaped_zombie_child() {
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
            !process_running(pid),
            "unreaped zombie should not count as a running process"
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
