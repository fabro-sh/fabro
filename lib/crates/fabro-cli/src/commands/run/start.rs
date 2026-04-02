use std::path::Path;

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use fabro_types::RunId;
use fabro_workflow::run_status::RunStatus;

use super::launcher::{
    LauncherRecord, active_launcher_record, launcher_log_path, launcher_record_path,
    remove_launcher_record, write_launcher_record,
};
use crate::store;

/// Spawn a detached engine process for the given run.
///
/// Returns the child process handle (use `.id()` for the PID).
pub(crate) async fn start_run(
    run_dir: &Path,
    run_id: &RunId,
    storage_dir: &Path,
    resume: bool,
) -> Result<std::process::Child> {
    if !resume {
        ensure_startable_run(storage_dir, run_id).await?;
    }
    let launcher_path = launcher_record_path(storage_dir, run_id);
    let log_path = launcher_log_path(storage_dir, run_id);

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = std::fs::File::create(&log_path)?;
    let stdout_log = log_file.try_clone()?;
    let exe = std::env::current_exe()?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["__detached", "--run-id"])
        .arg(run_id.to_string())
        .args(["--run-dir"])
        .arg(run_dir)
        .args(["--storage-dir"])
        .arg(storage_dir)
        .args(["--launcher-path"])
        .arg(&launcher_path);
    if resume {
        cmd.arg("--resume");
    }
    cmd.env_remove("FABRO_JSON");
    cmd.stdout(stdout_log)
        .stderr(log_file)
        .stdin(std::process::Stdio::null());

    #[cfg(unix)]
    fabro_proc::pre_exec_setsid(&mut cmd);

    let mut child = cmd.spawn()?;

    if let Err(err) = write_launcher_record(
        &launcher_path,
        &LauncherRecord {
            run_id: *run_id,
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

async fn ensure_startable_run(storage_dir: &Path, run_id: &RunId) -> Result<()> {
    if active_launcher_record(storage_dir, run_id).is_some() {
        bail!("an engine process is still running for this run — cannot start");
    }

    let run_store = store::open_run_reader(storage_dir, run_id)
        .await?
        .ok_or_else(|| anyhow!("Cannot start run: run {run_id} not found in store"))?;
    if let Some(record) = run_store.get_status().await? {
        if !matches!(record.status, RunStatus::Submitted | RunStatus::Starting) {
            bail!(
                "cannot start run: status is {:?}, expected submitted",
                record.status
            );
        }
    }

    Ok(())
}

fn kill_child_best_effort(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}
