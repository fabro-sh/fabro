use anyhow::bail;
use fabro_config::FabroSettingsExt;
use fabro_util::terminal::Styles;
use fabro_workflows::records::{RunRecord, RunRecordExt};
use fabro_workflows::run_lookup::{find_run_by_prefix, runs_base};

use crate::args::ResumeArgs;
use crate::cli_config::load_cli_settings;

/// Resume an interrupted workflow run.
///
/// Looks up the run by ID prefix, validates a checkpoint exists, cleans stale
/// artifacts from the previous execution, then spawns an engine subprocess
/// (identical to `fabro run`'s create→start→attach flow).
pub(crate) async fn resume_command(
    args: ResumeArgs,
    styles: &'static Styles,
) -> anyhow::Result<()> {
    let cli_settings = load_cli_settings(None)?;
    let base = runs_base(&cli_settings.storage_dir());
    let run_dir = find_run_by_prefix(&base, &args.run)?;

    // find_run_by_prefix can match orphan directories (no run.json).
    if !run_dir.join("run.json").exists() {
        bail!("run directory exists but has no run.json — cannot resume");
    }
    let run_id = RunRecord::load(&run_dir)?.run_id;

    if launcher_pid_alive(&run_dir) {
        bail!("an engine process is still running for this run — cannot resume");
    }

    let child = super::start::start_run(&run_dir, true)?;

    if args.detach {
        println!("{run_id}");
    } else {
        let exit_code = super::attach::attach_run(&run_dir, true, styles, Some(child)).await?;
        super::output::print_run_summary(&run_dir, &run_id, styles);
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }
    Ok(())
}

fn launcher_pid_alive(run_dir: &std::path::Path) -> bool {
    super::launcher::active_launcher_record_for_run(run_dir)
        .map(|record| process_alive(record.pid))
        .or_else(|| {
            std::fs::read_to_string(run_dir.join("run.pid"))
                .ok()
                .and_then(|pid| pid.trim().parse::<u32>().ok())
                .map(process_alive)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_pid_alive_returns_false_for_missing_record() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!launcher_pid_alive(dir.path()));
    }
}

#[allow(unsafe_code)]
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
