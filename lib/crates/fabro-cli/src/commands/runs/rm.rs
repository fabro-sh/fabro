use std::path::Path;

use anyhow::{Context, Result, bail};
use fabro_store::SlateStore;
use tracing::warn;

use crate::args::{GlobalArgs, RunsRemoveArgs};
use crate::shared::print_json_pretty;
use crate::store;
use crate::user_config::load_user_settings_with_globals;
use fabro_sandbox::reconnect::reconnect as reconnect_sandbox;
use fabro_workflow::event::{Event, append_event};
use fabro_workflow::run_lookup::RunInfo;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};

use super::short_run_id;

pub(crate) async fn remove_command(args: &RunsRemoveArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    remove_from(args, store.as_ref(), &base, globals).await
}

async fn remove_from(
    args: &RunsRemoveArgs,
    store: &SlateStore,
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

        if run.status().is_active() && !args.force {
            let run_id = run.run_id().to_string();
            let error = format!(
                "cannot remove active run {} (status: {}, use -f to force)",
                short_run_id(&run_id),
                run.status()
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

        let run_id = run.run_id().to_string();
        if let Err(err) = remove_run_dir_with_cleanup(store, &run).await {
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
        if let Err(err) = delete_run_store_state(store, &run).await {
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

pub(crate) async fn remove_run_with_cleanup(store: &SlateStore, run: &RunInfo) -> Result<()> {
    remove_run_dir_with_cleanup(store, run).await?;
    delete_run_store_state(store, run).await
}

async fn remove_run_dir_with_cleanup(store: &SlateStore, run: &RunInfo) -> Result<()> {
    let run_id = run.run_id();
    let run_store = match store.open_run_reader(&run_id).await {
        Ok(run_store) => Some(run_store),
        Err(err) => {
            warn!(
                run_id = %run_id,
                error = %err,
                "failed to open run store during removal"
            );
            None
        }
    };
    if let Some(run_store) = run_store.as_ref() {
        if let Err(err) =
            append_event(run_store, &run_id, &Event::RunRemoving { reason: None }).await
        {
            warn!(
                run_id = %run_id,
                error = %err,
                "failed to append removing status event"
            );
        }
    }

    if let Some(record) = load_sandbox_record(&run.path, run_store.as_ref()).await {
        if record.provider != "local" {
            match reconnect_sandbox(&record).await {
                Ok(sandbox) => {
                    if let Err(err) = sandbox.cleanup().await {
                        warn!(run_id = %run_id, error = %err, "sandbox cleanup failed");
                    }
                }
                Err(err) => {
                    warn!(run_id = %run_id, error = %err, "sandbox reconnect failed");
                }
            }
        }
    }

    std::fs::remove_dir_all(&run.path)
        .with_context(|| format!("failed to delete {}", run.path.display()))
}

async fn delete_run_store_state(store: &SlateStore, run: &RunInfo) -> Result<()> {
    store
        .delete_run(&run.run_id())
        .await
        .with_context(|| format!("failed to delete store state for {}", run.run_id()))
}

async fn load_sandbox_record(
    _run_dir: &Path,
    run_store: Option<&fabro_store::SlateRunStore>,
) -> Option<fabro_sandbox::SandboxRecord> {
    if let Some(run_store) = run_store {
        match run_store.state().await {
            Ok(state) => return state.sandbox,
            Err(err) => {
                warn!(error = %err, "failed to load sandbox record from store");
            }
        }
    }
    None
}
