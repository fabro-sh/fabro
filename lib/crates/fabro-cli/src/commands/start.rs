use std::path::Path;

use anyhow::{bail, Result};

/// Spawn a detached engine process for the given run directory.
///
/// The engine process reads `spec.json` from the run directory and executes the
/// workflow. Returns the child process PID.
pub fn start_run(run_dir: &Path) -> Result<u32> {
    // Validate status is Submitted
    let status_path = run_dir.join("status.json");
    match fabro_workflows::run_status::RunStatusRecord::load(&status_path) {
        Ok(record) if record.status != fabro_workflows::run_status::RunStatus::Submitted => {
            bail!(
                "Cannot start run: status is {:?}, expected Submitted",
                record.status
            );
        }
        _ => {} // No status file or Submitted — proceed
    }

    // Validate spec.json is loadable
    fabro_workflows::run_spec::RunSpec::load(run_dir)
        .map_err(|e| anyhow::anyhow!("Cannot start run: failed to load spec.json: {e}"))?;

    // Write Starting status before spawning to prevent duplicate engines
    fabro_workflows::run_status::write_run_status(
        run_dir,
        fabro_workflows::run_status::RunStatus::Starting,
        None,
    );

    let log_file = std::fs::File::create(run_dir.join("detach.log"))?;

    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["_run_engine", "--run-dir"])
        .arg(run_dir)
        .stdout(log_file.try_clone()?)
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

    let child = cmd.spawn()?;
    let pid = child.id();

    // Write PID file
    std::fs::write(run_dir.join("run.pid"), pid.to_string())?;

    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_workflows::run_spec::RunSpec;
    use fabro_workflows::run_status::{write_run_status, RunStatus, RunStatusRecord};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn sample_spec() -> RunSpec {
        RunSpec {
            run_id: "run-test123".to_string(),
            workflow_path: PathBuf::from("/tmp/test-workflow.toml"),
            dot_source: "digraph { a -> b }".to_string(),
            working_directory: PathBuf::from("/tmp"),
            goal: None,
            model: "claude-sonnet-4-20250514".to_string(),
            provider: Some("anthropic".to_string()),
            sandbox_provider: "local".to_string(),
            labels: HashMap::new(),
            verbose: false,
            no_retro: true,
            ssh: false,
            preserve_sandbox: false,
            dry_run: false,
            auto_approve: true,
            resume: None,
            run_branch: None,
        }
    }

    // Bug 5: start_run should write Starting status before spawning engine
    // to prevent duplicate engine processes from concurrent start calls.
    #[test]
    fn bug5_start_run_writes_starting_status_before_spawn() {
        let dir = tempfile::tempdir().unwrap();
        write_run_status(dir.path(), RunStatus::Submitted, None);
        sample_spec().save(dir.path()).unwrap();

        // start_run may fail on spawn (test binary != fabro), but we only
        // care about the status file being updated before the spawn attempt.
        let _ = start_run(dir.path());

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(
            record.status,
            RunStatus::Starting,
            "start_run should write Starting status before spawning to prevent duplicate engines"
        );
    }
}
