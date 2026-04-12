/// Check whether a process with the given PID is alive.
///
/// On Unix, sends signal 0 via `kill(2)`. Returns `false` if the pid does not
/// fit in `i32`. On non-Unix platforms, conservatively returns `true`.
pub fn process_alive(pid: u32) -> bool {
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
