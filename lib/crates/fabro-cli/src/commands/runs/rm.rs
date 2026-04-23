use anyhow::{Result, bail};
use fabro_types::settings::cli::OutputFormat;

use super::short_run_id;
use crate::args::RunsRemoveArgs;
use crate::command_context::CommandContext;
use crate::server_client;
use crate::shared::print_json_pretty;

pub(crate) async fn remove_command(args: &RunsRemoveArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    remove_from(
        args,
        ctx.server().await?.as_ref(),
        ctx.user_settings().cli.output.format == OutputFormat::Json,
        ctx.printer(),
    )
    .await
}

async fn remove_from(
    args: &RunsRemoveArgs,
    client: &server_client::Client,
    json: bool,
    printer: fabro_util::printer::Printer,
) -> Result<()> {
    let mut had_errors = false;
    let mut removed = Vec::new();
    let mut errors = Vec::new();

    for identifier in &args.runs {
        let run = match client.resolve_run(identifier).await {
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

        let run_id = run.run_id.to_string();
        if let Err(err) = delete_server_run(client, &run.run_id, args.force).await {
            let error = err.to_string();
            if !json {
                if error.starts_with("cannot remove active run ") {
                    fabro_util::printerr!(printer, "{error}");
                } else {
                    fabro_util::printerr!(printer, "error: {identifier}: {error}");
                }
            }
            errors.push(serde_json::json!({
                "identifier": identifier,
                "error": error,
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
    client: &server_client::Client,
    run_id: &fabro_types::RunId,
    force: bool,
) -> Result<()> {
    client.delete_store_run(run_id, force).await
}
