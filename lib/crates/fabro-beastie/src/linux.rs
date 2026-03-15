use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

/// Linux sleep inhibitor using `systemd-inhibit` (preferred) or
/// `gnome-session-inhibit` (fallback).
///
/// Spawns an inhibitor child process that blocks idle sleep as long as it is
/// alive. On `Drop`, the child is killed to release the inhibition.
pub(crate) struct LinuxGuard {
    child: Child,
}

impl LinuxGuard {
    pub(crate) fn acquire() -> Option<Self> {
        // Try systemd-inhibit first
        if let Some(guard) = Self::try_systemd_inhibit() {
            return Some(guard);
        }
        // Fallback to gnome-session-inhibit
        if let Some(guard) = Self::try_gnome_inhibit() {
            return Some(guard);
        }
        tracing::warn!(
            "Sleep inhibitor: no supported inhibitor found \
             (tried systemd-inhibit, gnome-session-inhibit)"
        );
        None
    }

    fn try_systemd_inhibit() -> Option<Self> {
        let child = unsafe {
            Command::new("systemd-inhibit")
                .args([
                    "--what=idle",
                    "--who=fabro",
                    "--why=Workflow in progress",
                    "--mode=block",
                    "sleep",
                    "infinity",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .pre_exec(|| {
                    // Ensure the child is killed if the parent dies unexpectedly.
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                })
                .spawn()
        };
        match child {
            Ok(child) => {
                let pid = child.id();
                tracing::debug!(pid, "Sleep inhibitor: systemd-inhibit started");
                Some(Self { child })
            }
            Err(e) => {
                tracing::debug!(%e, "systemd-inhibit not available");
                None
            }
        }
    }

    fn try_gnome_inhibit() -> Option<Self> {
        let child = unsafe {
            Command::new("gnome-session-inhibit")
                .args([
                    "--inhibit",
                    "idle",
                    "--reason",
                    "Workflow in progress",
                    "sleep",
                    "infinity",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                })
                .spawn()
        };
        match child {
            Ok(child) => {
                let pid = child.id();
                tracing::debug!(pid, "Sleep inhibitor: gnome-session-inhibit started");
                Some(Self { child })
            }
            Err(e) => {
                tracing::debug!(%e, "gnome-session-inhibit not available");
                None
            }
        }
    }
}

impl Drop for LinuxGuard {
    fn drop(&mut self) {
        let pid = self.child.id();
        let _ = self.child.kill();
        let _ = self.child.wait();
        tracing::debug!(pid, "Sleep inhibitor: linux inhibitor child killed");
    }
}
