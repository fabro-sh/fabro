use std::process::{Child, Command};
use tracing::{debug, warn};

pub(crate) struct LinuxSleepInhibitor {
    child: Child,
}

impl LinuxSleepInhibitor {
    pub(crate) fn acquire() -> Option<Self> {
        // Try systemd-inhibit first, then gnome-session-inhibit as fallback
        if let Some(inhibitor) = Self::try_systemd_inhibit() {
            return Some(inhibitor);
        }
        if let Some(inhibitor) = Self::try_gnome_inhibit() {
            return Some(inhibitor);
        }
        warn!("Sleep inhibitor: no supported inhibitor found on this system");
        None
    }

    fn try_systemd_inhibit() -> Option<Self> {
        let result = Command::new("systemd-inhibit")
            .args([
                "--what=idle",
                "--mode=block",
                "--who=fabro",
                "--reason=Fabro workflow running",
                "sleep",
                "infinity",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match result {
            Ok(mut child) => {
                // Set PR_SET_PDEATHSIG so the child is killed if the parent dies
                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::process::CommandExt;
                    // The child is already spawned, but we can set pdeathsig via /proc
                    // Actually, PR_SET_PDEATHSIG must be set from within the child process.
                    // For a pre-spawned child, we rely on explicit Drop cleanup.
                    // The safer approach is to use pre_exec, so let's re-spawn.
                    let _ = child.kill();
                    let _ = child.wait();

                    let result = unsafe {
                        Command::new("systemd-inhibit")
                            .args([
                                "--what=idle",
                                "--mode=block",
                                "--who=fabro",
                                "--reason=Fabro workflow running",
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
                    match result {
                        Ok(child) => {
                            debug!("Sleep inhibitor: acquired via systemd-inhibit");
                            Some(Self { child })
                        }
                        Err(e) => {
                            warn!("Sleep inhibitor: failed to respawn systemd-inhibit: {e}");
                            None
                        }
                    }
                }

                #[cfg(not(target_os = "linux"))]
                {
                    debug!("Sleep inhibitor: acquired via systemd-inhibit");
                    Some(Self { child })
                }
            }
            Err(e) => {
                debug!("Sleep inhibitor: systemd-inhibit not available: {e}");
                None
            }
        }
    }

    fn try_gnome_inhibit() -> Option<Self> {
        let result = Command::new("gnome-session-inhibit")
            .args([
                "--inhibit=idle",
                "--reason",
                "Fabro workflow running",
                "sleep",
                "infinity",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match result {
            Ok(child) => {
                debug!("Sleep inhibitor: acquired via gnome-session-inhibit");
                Some(Self { child })
            }
            Err(e) => {
                debug!("Sleep inhibitor: gnome-session-inhibit not available: {e}");
                None
            }
        }
    }
}

impl Drop for LinuxSleepInhibitor {
    fn drop(&mut self) {
        debug!("Sleep inhibitor: releasing (killing inhibitor child process)");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
