use anyhow::bail;
use fabro_config::FabroSettingsExt;
use fabro_util::terminal::Styles;
use fabro_workflow::records::{RunRecord, RunRecordExt};
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};

use crate::args::{GlobalArgs, ResumeArgs};
use crate::shared::print_json_pretty;
use crate::store;
use crate::user_config::load_user_settings_with_globals;

/// Resume an interrupted workflow run.
///
/// Looks up the run by ID prefix, validates a checkpoint exists, cleans stale
/// artifacts from the previous execution, then spawns an engine subprocess
/// (identical to `fabro run`'s create→start→attach flow).
pub(crate) async fn resume_command(
    args: ResumeArgs,
    styles: &'static Styles,
    globals: &GlobalArgs,
) -> anyhow::Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;
    let run_dir = run.path;

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
        if globals.json {
            print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
        } else {
            println!("{run_id}");
        }
    } else {
        let exit_code = super::attach::attach_run(
            &run_dir,
            Some(&run_id),
            true,
            styles,
            Some(child),
            globals.json,
        )
        .await?;
        if !globals.json {
            super::output::print_run_summary(&run_dir, run_id, styles);
        }
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }
    Ok(())
}

fn launcher_pid_alive(run_dir: &std::path::Path) -> bool {
    super::launcher::active_launcher_record_for_run(run_dir)
        .is_some_and(|record| process_alive(record.pid))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_pid_alive_returns_false_for_missing_record() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!launcher_pid_alive(dir.path()));
    }
}
