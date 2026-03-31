use std::path::Path;

use anyhow::{Context, Result, bail};
use fabro_config::FabroSettingsExt;
use fabro_sandbox::SandboxRecordExt;
use fabro_store::Store;
use tracing::warn;

use fabro_sandbox::reconnect::reconnect as reconnect_sandbox;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
use fabro_workflow::run_status::{RunStatus, RunStatusRecord, write_run_status};

use crate::args::{GlobalArgs, RunsRemoveArgs};
use crate::shared::print_json_pretty;
use crate::store;
use crate::user_config::load_user_settings_with_globals;

use super::short_run_id;

pub(crate) async fn remove_command(args: &RunsRemoveArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    remove_from(args, store.as_ref(), &base, globals).await
}

async fn remove_from(
    args: &RunsRemoveArgs,
    store: &dyn Store,
    base: &Path,
    globals: &GlobalArgs,
) -> Result<()> {
    let mut had_errors = false;
    let mut removed = Vec::new();
    let mut errors = Vec::new();

    for identifier in &args.runs {
        let run = match resolve_run_combined(store, base, identifier).await {
            Ok(run) => run,
            Err(err) => {
                if !globals.json {
                    eprintln!("error: {identifier}: {err}");
                }
                errors.push(serde_json::json!({
                    "identifier": identifier,
                    "error": err.to_string(),
                }));
                had_errors = true;
                continue;
            }
        };

        if run.status.is_active() && !args.force {
            let run_id = run.run_id.to_string();
            let error = format!(
                "cannot remove active run {} (status: {}, use -f to force)",
                short_run_id(&run_id),
                run.status
            );
            if !globals.json {
                eprintln!("{error}");
            }
            errors.push(serde_json::json!({
                "identifier": identifier,
                "error": error,
            }));
            had_errors = true;
            continue;
        }

        write_run_status(&run.path, RunStatus::Removing, None);
        if let Ok(Some(run_store)) = store.open_run_reader(&run.run_id).await {
            if let Err(err) = run_store
                .put_status(&RunStatusRecord::new(RunStatus::Removing, None))
                .await
            {
                warn!(
                    run_id = %run.run_id,
                    error = %err,
                    "failed to save removing status to store"
                );
            }
        }

        let sandbox_path = run.path.join("sandbox.json");
        if let Ok(record) = fabro_sandbox::SandboxRecord::load(&sandbox_path) {
            if record.provider != "local" {
                match reconnect_sandbox(&record).await {
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

        let run_id = run.run_id.to_string();
        if let Err(err) = std::fs::remove_dir_all(&run.path)
            .with_context(|| format!("failed to delete {}", run.path.display()))
        {
            if !globals.json {
                eprintln!("error: {identifier}: {err}");
            }
            errors.push(serde_json::json!({
                "identifier": identifier,
                "error": err.to_string(),
            }));
            had_errors = true;
            continue;
        }
        removed.push(run_id.clone());
        if !globals.json {
            eprintln!("{}", short_run_id(&run_id));
        }
        if let Err(err) = store
            .delete_run(&run.run_id)
            .await
            .with_context(|| format!("failed to delete store state for {}", run.run_id))
        {
            if !globals.json {
                eprintln!("error: {identifier}: {err}");
            }
            errors.push(serde_json::json!({
                "identifier": identifier,
                "error": err.to_string(),
            }));
            had_errors = true;
        }
    }

    if globals.json {
        print_json_pretty(&serde_json::json!({
            "removed": removed,
            "errors": errors,
        }))?;
    }

    if had_errors {
        bail!("some runs could not be removed");
    }
    Ok(())
}
