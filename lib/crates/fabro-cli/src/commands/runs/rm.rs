use anyhow::{Context, Result, bail};

use super::short_run_id;
use crate::args::{GlobalArgs, RunsRemoveArgs};
use crate::command_context::CommandContext;
use crate::server_client;
use crate::server_runs::{
    ServerRunSummaryInfo, ServerSummaryLookup, resolve_server_run_from_summaries,
};
use crate::shared::print_json_pretty;

pub(crate) async fn remove_command(args: &RunsRemoveArgs, globals: &GlobalArgs) -> Result<()> {
    let ctx = CommandContext::for_target(&args.server)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    remove_from(args, lookup.client(), lookup.runs(), globals).await
}

async fn remove_from(
    args: &RunsRemoveArgs,
    client: &server_client::ServerStoreClient,
    runs: &[ServerRunSummaryInfo],
    globals: &GlobalArgs,
) -> Result<()> {
    let mut had_errors = false;
    let mut removed = Vec::new();
    let mut errors = Vec::new();

    for identifier in &args.runs {
        let run = match resolve_server_run_from_summaries(runs, identifier) {
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
        if let Err(err) = delete_server_run(client, &run).await {
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

async fn delete_server_run(
    client: &server_client::ServerStoreClient,
    run: &ServerRunSummaryInfo,
) -> Result<()> {
    client
        .delete_store_run(&run.run_id())
        .await
        .with_context(|| format!("failed to delete store state for {}", run.run_id()))
}
