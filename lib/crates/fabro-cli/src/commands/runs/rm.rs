use anyhow::{Context, Result, bail};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;

use super::short_run_id;
use crate::args::RunsRemoveArgs;
use crate::command_context::CommandContext;
use crate::server_client;
use crate::server_runs::{
    ServerRunSummaryInfo, ServerSummaryLookup, resolve_server_run_from_summaries,
};
use crate::shared::print_json_pretty;

pub(crate) async fn remove_command(
    args: &RunsRemoveArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    remove_from(args, lookup.client(), lookup.runs(), cli, printer).await
}

async fn remove_from(
    args: &RunsRemoveArgs,
    client: &server_client::ServerStoreClient,
    runs: &[ServerRunSummaryInfo],
    cli: &CliSettings,
    printer: Printer,
) -> Result<()> {
    let json = cli.output.format == OutputFormat::Json;
    let mut had_errors = false;
    let mut removed = Vec::new();
    let mut errors = Vec::new();

    for identifier in &args.runs {
        let run = match resolve_server_run_from_summaries(runs, identifier) {
            Ok(run) => run,
            Err(err) => {
                if !json {
                    fabro_util::printerr!(printer, "error: {identifier}: {err}");
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
            if !json {
                fabro_util::printerr!(printer, "{error}");
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
            if !json {
                fabro_util::printerr!(printer, "error: {identifier}: {err}");
            }
            errors.push(serde_json::json!({
                "identifier": identifier,
                "error": err.to_string(),
            }));
            had_errors = true;
            continue;
        }
        removed.push(run_id.clone());
        if !json {
            fabro_util::printerr!(printer, "{}", short_run_id(&run_id));
        }
    }

    if json {
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
