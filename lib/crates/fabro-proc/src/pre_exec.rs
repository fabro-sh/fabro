use std::os::unix::process::CommandExt;

/// Register a `pre_exec` hook that calls `setsid()` to detach the child
/// into its own session.
pub fn pre_exec_setsid(cmd: &mut impl CommandExt) {
    // SAFETY: setsid() is async-signal-safe per POSIX.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// Register a `pre_exec` hook that calls `setpgid(0, 0)` to place the
/// child in its own process group.
pub fn pre_exec_setpgid(cmd: &mut impl CommandExt) {
    // SAFETY: setpgid() is async-signal-safe per POSIX.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}

/// Register a `pre_exec` hook that calls `prctl(PR_SET_PDEATHSIG, SIGTERM)`
/// so the child receives SIGTERM when its parent dies.
#[cfg(target_os = "linux")]
pub fn pre_exec_pdeathsig(cmd: &mut impl CommandExt) {
    // SAFETY: prctl(PR_SET_PDEATHSIG, ...) is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
}
