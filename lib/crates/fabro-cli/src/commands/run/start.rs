use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::Utc;
use fabro_config::FabroSettingsExt;
use fabro_workflow::records::{RunRecord, RunRecordExt};

use super::launcher::{
    LauncherRecord, launcher_log_path, launcher_record_path, remove_launcher_record,
    write_launcher_record,
};

/// Spawn a detached engine process for the given run directory.
///
/// The engine process reads `run.json` from the run directory and executes the
/// workflow. Returns the child process handle (use `.id()` for the PID).
#[allow(unsafe_code)]
pub(crate) fn start_run(run_dir: &Path, resume: bool) -> Result<std::process::Child> {
    let record = RunRecord::load(run_dir)
        .map_err(|e| anyhow!("Cannot start run: failed to load run.json: {e}"))?;

    let storage_dir = record.settings.storage_dir();
    let launcher_path = launcher_record_path(&storage_dir, &record.run_id);
    let log_path = launcher_log_path(&storage_dir, &record.run_id);

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = std::fs::File::create(&log_path)?;
    let stdout_log = log_file.try_clone()?;
    let exe = std::env::current_exe()?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["__detached", "--run-dir"])
        .arg(run_dir)
        .args(["--launcher-path"])
        .arg(&launcher_path);
    if resume {
        cmd.arg("--resume");
    }
    cmd.stdout(stdout_log)
        .stderr(log_file)
        .stdin(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let mut child = cmd.spawn()?;

    if let Err(err) = write_launcher_record(
        &launcher_path,
        &LauncherRecord {
            run_id: record.run_id,
            run_dir: run_dir.to_path_buf(),
            pid: child.id(),
            resume,
            log_path,
            started_at: Utc::now(),
        },
    ) {
        kill_child_best_effort(&mut child);
        return Err(err);
    }

    if matches!(child.try_wait(), Ok(Some(_))) {
        remove_launcher_record(&launcher_path);
    }

    Ok(child)
}

fn kill_child_best_effort(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}
