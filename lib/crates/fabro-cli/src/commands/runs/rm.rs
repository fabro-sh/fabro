use std::path::Path;

use anyhow::{bail, Context, Result};
use fabro_config::FabroSettingsExt;
use fabro_sandbox::SandboxRecordExt;
use tracing::warn;

use crate::args::RunsRemoveArgs;

use super::short_run_id;

pub async fn remove_command(args: &RunsRemoveArgs) -> Result<()> {
    let cli_config = crate::cli_config::load_cli_settings(None)?;
    let base = fabro_workflows::run_lookup::runs_base(&cli_config.storage_dir());
    remove_from(args, &base).await
}

async fn remove_from(args: &RunsRemoveArgs, base: &Path) -> Result<()> {
    let mut had_errors = false;

    for identifier in &args.runs {
        let run = match fabro_workflows::run_lookup::resolve_run(base, identifier) {
            Ok(run) => run,
            Err(err) => {
                eprintln!("error: {identifier}: {err}");
                had_errors = true;
                continue;
            }
        };

        if run.status.is_active() && !args.force {
            eprintln!(
                "cannot remove active run {} (status: {}, use -f to force)",
                short_run_id(&run.run_id),
                run.status
            );
            had_errors = true;
            continue;
        }

        fabro_workflows::run_status::write_run_status(
            &run.path,
            fabro_workflows::run_status::RunStatus::Removing,
            None,
        );

        let sandbox_path = run.path.join("sandbox.json");
        if let Ok(record) = fabro_sandbox::SandboxRecord::load(&sandbox_path) {
            if record.provider != "local" {
                match fabro_sandbox::reconnect::reconnect(&record).await {
                    Ok(sandbox) => {
                        if let Err(err) = sandbox.cleanup().await {
                            warn!(run_id = %run.run_id, error = %err, "sandbox cleanup failed");
                        }
                    }
                    Err(err) => {
                        warn!(run_id = %run.run_id, error = %err, "sandbox reconnect failed");
                    }
                }
            }
        }

        std::fs::remove_dir_all(&run.path)
            .with_context(|| format!("failed to delete {}", run.path.display()))?;
        eprintln!("{}", short_run_id(&run.run_id));
    }

    if had_errors {
        bail!("some runs could not be removed");
    }
    Ok(())
}
