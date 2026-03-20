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
