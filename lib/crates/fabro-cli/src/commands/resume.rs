use anyhow::bail;
use fabro_util::terminal::Styles;
use fabro_workflows::records::{Checkpoint, RunRecord};
use fabro_workflows::run_status::{RunStatus, RunStatusRecord};

use crate::args::ResumeArgs;

/// Resume an interrupted workflow run.
///
/// Looks up the run by ID prefix, validates a checkpoint exists, cleans stale
/// artifacts from the previous execution, then spawns an engine subprocess
/// (identical to `fabro run`'s create→start→attach flow).
pub async fn resume_command(args: ResumeArgs, styles: &'static Styles) -> anyhow::Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    let run_dir = fabro_workflows::run_lookup::find_run_by_prefix(&base, &args.run)?;

    // find_run_by_prefix can match orphan directories (no run.json).
    if !run_dir.join("run.json").exists() {
        bail!("run directory exists but has no run.json — cannot resume");
    }
    let run_id = RunRecord::load(&run_dir)?.run_id;

    // Guard against resuming a live run — must happen before checkpoint
    // validation because the engine writes checkpoint.json with a plain
    // fs::write, so a mid-write read would see a truncated file and
    // report "corrupt" for a run that is simply still alive.
    if is_pid_alive(&run_dir.join("run.pid")) {
        bail!("an engine process is still running for this run — cannot resume");
    }

    // Reject runs that completed successfully — only failed/interrupted
    // runs should be resumed.
    if let Ok(record) = RunStatusRecord::load(&run_dir.join("status.json")) {
        if record.status == RunStatus::Succeeded {
            bail!("run already succeeded — nothing to resume");
        }
    }

    // Validate checkpoint is parseable before touching any state.
    // A crash during the original run can leave a truncated file;
    // we must not destroy the old conclusion/failure evidence and
    // only then discover the checkpoint is corrupt.
    let cp_path = run_dir.join("checkpoint.json");
    Checkpoint::load(&cp_path).map_err(|e| {
        if cp_path.exists() {
            anyhow::anyhow!("checkpoint.json is corrupt — cannot resume: {e}")
        } else {
            anyhow::anyhow!("no checkpoint found — nothing to resume")
        }
    })?;

    // Clean stale artifacts from previous execution
    for name in &[
        "conclusion.json",
        "pull_request.json",
        "detached_failure.json",
        "interview_request.json",
        "interview_response.json",
        "interview_request.claim",
        "detach.log",
        "run.pid",
        "progress.jsonl",
    ] {
        let _ = std::fs::remove_file(run_dir.join(name));
    }

    // Reset status for re-execution
    fabro_workflows::run_status::write_run_status(&run_dir, RunStatus::Submitted, None);

    let child = super::start::start_run(&run_dir, true)?;

    if args.detach {
        println!("{run_id}");
    } else {
        let exit_code = super::attach::attach_run(&run_dir, true, styles, Some(child)).await?;
        super::run::print_run_summary(&run_dir, &run_id, styles);
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Check whether a PID file contains a live process.
fn is_pid_alive(pid_path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(pid_path) else {
        return false;
    };
    let Ok(pid) = content.trim().parse::<i32>() else {
        return false;
    };
    // kill(pid, 0) checks liveness without sending a signal
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pid_alive_returns_false_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_pid_alive(&dir.path().join("run.pid")));
    }

    #[test]
    fn is_pid_alive_returns_false_for_invalid_pid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run.pid"), "not-a-pid").unwrap();
        assert!(!is_pid_alive(&dir.path().join("run.pid")));
    }

    #[test]
    fn is_pid_alive_returns_true_for_current_process() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        std::fs::write(dir.path().join("run.pid"), pid.to_string()).unwrap();
        assert!(is_pid_alive(&dir.path().join("run.pid")));
    }
}
