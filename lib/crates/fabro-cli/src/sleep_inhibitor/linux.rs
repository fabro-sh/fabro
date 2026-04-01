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

    /// Spawn a command with `PR_SET_PDEATHSIG` so the child is automatically
    /// killed if the parent process dies (prevents orphan `sleep infinity`).
    fn spawn_with_pdeathsig(cmd: &mut Command) -> std::io::Result<Child> {
        fabro_proc::pre_exec_pdeathsig(cmd);
        cmd.spawn()
    }

    fn try_systemd_inhibit() -> Option<Self> {
        let mut cmd = Command::new("systemd-inhibit");
        cmd.args([
            "--what=idle",
            "--mode=block",
            "--who=fabro",
            "--reason=Fabro workflow running",
            "sleep",
            "infinity",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

        match Self::spawn_with_pdeathsig(&mut cmd) {
            Ok(child) => {
                debug!("Sleep inhibitor: acquired via systemd-inhibit");
                Some(Self { child })
            }
            Err(e) => {
                debug!("Sleep inhibitor: systemd-inhibit not available: {e}");
                None
            }
        }
    }

    fn try_gnome_inhibit() -> Option<Self> {
        let mut cmd = Command::new("gnome-session-inhibit");
        cmd.args([
            "--inhibit=idle",
            "--reason",
            "Fabro workflow running",
            "sleep",
            "infinity",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

        match Self::spawn_with_pdeathsig(&mut cmd) {
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
