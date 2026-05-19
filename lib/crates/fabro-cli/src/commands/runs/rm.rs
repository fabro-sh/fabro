use anyhow::{Result, bail};

use crate::args::RunsRemoveArgs;
use crate::command_context::CommandContext;
use crate::server_client;
use crate::shared::print_json_pretty;

pub(crate) async fn remove_command(args: &RunsRemoveArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    remove_from(args, &ctx).await
}

async fn remove_from(args: &RunsRemoveArgs, ctx: &CommandContext) -> Result<()> {
    let client = ctx.server().await?;
    let client = client.as_ref();
    let json = ctx.json_output();
    let printer = ctx.printer();
    let mut had_errors = false;
    let mut removed = Vec::new();
    let mut errors = Vec::new();

    for identifier in &args.runs {
        let run_id = match resolve_target(client, identifier, args.force).await {
            Ok(run_id) => run_id,
            Err(err) => {
                let error = err.to_string();
                if !json {
                    fabro_util::printerr!(printer, "error: {identifier}: {error}");
                }
                errors.push(serde_json::json!({
                    "identifier": identifier,
                    "error": error,
                }));
                had_errors = true;
                continue;
            }
        };

        if let Err(err) = delete_server_run(client, &run_id, args.force).await {
            let error = err.to_string();
            if !json {
                if !args.force && error.starts_with("cannot remove active run ") {
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

        let run_id_string = run_id.to_string();
        removed.push(run_id_string.clone());
        if !json {
            fabro_util::printerr!(printer, "{run_id_string}");
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

async fn resolve_target(
    client: &server_client::Client,
    identifier: &str,
    force: bool,
) -> Result<fabro_types::RunId> {
    if force {
        if let Ok(run_id) = identifier.parse::<fabro_types::RunId>() {
            return Ok(run_id);
        }
    }
    Ok(client.resolve_run(identifier).await?.id)
}

async fn delete_server_run(
    client: &server_client::Client,
    run_id: &fabro_types::RunId,
    force: bool,
) -> Result<()> {
    client.delete_store_run(run_id, force).await
}
