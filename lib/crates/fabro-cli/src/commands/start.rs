use std::path::Path;

use anyhow::{bail, Result};
use fabro_workflows::run_status::{RunStatus, StatusReason};

use super::detached_support::persist_detached_failure;

/// Spawn a detached engine process for the given run directory.
///
/// The engine process reads `run.json` from the run directory and executes the
/// workflow. Returns the child process handle (use `.id()` for the PID).
pub fn start_run(run_dir: &Path, resume: bool) -> Result<std::process::Child> {
    // Validate status is Submitted
    let status_path = run_dir.join("status.json");
    match fabro_workflows::run_status::RunStatusRecord::load(&status_path) {
        Ok(record) if record.status != RunStatus::Submitted => {
            bail!(
                "Cannot start run: status is {:?}, expected Submitted",
                record.status
            );
        }
        _ => {} // No status file or Submitted — proceed
    }

    // Validate run.json is loadable
    fabro_workflows::records::RunRecord::load(run_dir)
        .map_err(|e| anyhow::anyhow!("Cannot start run: failed to load run.json: {e}"))?;

    // Write Starting status before spawning to prevent duplicate engines
    fabro_workflows::run_status::write_run_status(run_dir, RunStatus::Starting, None);

    let log_file = match std::fs::File::create(run_dir.join("detach.log")) {
        Ok(file) => file,
        Err(err) => {
            let err = err.into();
            let _ = persist_detached_failure(run_dir, "launch", StatusReason::LaunchFailed, &err);
            return Err(err);
        }
    };

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            let err = err.into();
            let _ = persist_detached_failure(run_dir, "launch", StatusReason::LaunchFailed, &err);
            return Err(err);
        }
    };

    let mut cmd = std::process::Command::new(&exe);
    let stdout_log = match log_file.try_clone() {
        Ok(file) => file,
        Err(err) => {
            let err = err.into();
            let _ = persist_detached_failure(run_dir, "launch", StatusReason::LaunchFailed, &err);
            return Err(err);
        }
    };
    cmd.args(["_run_engine", "--run-dir"]).arg(run_dir);
    if resume {
        cmd.arg("--resume");
    }
    cmd.stdout(stdout_log)
        .stderr(log_file)
        .stdin(std::process::Stdio::null());

    // Detach from the controlling terminal on unix
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

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            let err = err.into();
            let _ = persist_detached_failure(run_dir, "launch", StatusReason::LaunchFailed, &err);
            return Err(err);
        }
    };

    // Write PID file
    if let Err(err) = std::fs::write(run_dir.join("run.pid"), child.id().to_string()) {
        kill_child_best_effort(&mut child);
        let err = err.into();
        let _ = persist_detached_failure(run_dir, "launch", StatusReason::LaunchFailed, &err);
        return Err(err);
    }

    Ok(child)
}

fn kill_child_best_effort(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use fabro_config::config::FabroConfig;
    use fabro_graphviz::graph::Graph;
    use fabro_workflows::records::RunRecord;
    use fabro_workflows::run_status::{write_run_status, RunStatus, RunStatusRecord, StatusReason};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn sample_record() -> RunRecord {
        RunRecord {
            run_id: "run-test123".to_string(),
            created_at: Utc::now(),
            config: FabroConfig::default(),
            graph: Graph {
                name: "test".to_string(),
                ..Default::default()
            },
            workflow_slug: None,
            working_directory: PathBuf::from("/tmp"),
            host_repo_path: None,
            base_branch: None,
            labels: HashMap::new(),
        }
    }

    #[test]
    fn start_run_marks_failed_when_spawn_cannot_start_engine() {
        let dir = tempfile::tempdir().unwrap();
        write_run_status(dir.path(), RunStatus::Submitted, None);
        sample_record().save(dir.path()).unwrap();
        std::fs::create_dir(dir.path().join("detach.log")).unwrap();

        let _ = start_run(dir.path(), false);

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(
            record.status,
            RunStatus::Failed,
            "start_run should persist a terminal failure on launch errors"
        );
        assert_eq!(record.reason, Some(StatusReason::LaunchFailed));
        assert!(dir.path().join("conclusion.json").exists());
        let progress = std::fs::read_to_string(dir.path().join("progress.jsonl")).unwrap();
        assert!(progress.contains("launch_failed"));
    }
}
