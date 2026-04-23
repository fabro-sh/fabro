use anyhow::{Result, bail};
use fabro_types::settings::cli::OutputFormat;

use super::short_run_id;
use crate::args::{RunsArchiveArgs, RunsUnarchiveArgs};
use crate::command_context::CommandContext;
use crate::server_client;
use crate::shared::print_json_pretty;

pub(crate) async fn archive_command(
    args: &RunsArchiveArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    run_bulk(
        Action::Archive,
        &args.runs,
        ctx.server().await?.as_ref(),
        ctx.user_settings().cli.output.format == OutputFormat::Json,
        ctx.printer(),
    )
    .await
}

pub(crate) async fn unarchive_command(
    args: &RunsUnarchiveArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    run_bulk(
        Action::Unarchive,
        &args.runs,
        ctx.server().await?.as_ref(),
        ctx.user_settings().cli.output.format == OutputFormat::Json,
        ctx.printer(),
    )
    .await
}

#[derive(Clone, Copy)]
enum Action {
    Archive,
    Unarchive,
}

impl Action {
    fn past(self) -> &'static str {
        match self {
            Self::Archive => "archived",
            Self::Unarchive => "unarchived",
        }
    }

    fn json_key(self) -> &'static str {
        self.past()
    }
}

async fn run_bulk(
    action: Action,
    identifiers: &[String],
    client: &server_client::Client,
    json: bool,
    printer: fabro_util::printer::Printer,
) -> Result<()> {
    let mut had_errors = false;
    let mut changed = Vec::new();
    let mut errors = Vec::new();

    for identifier in identifiers {
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

        let run_id = run.run_id;
        let result = match action {
            Action::Archive => client.archive_run(&run_id).await,
            Action::Unarchive => client.unarchive_run(&run_id).await,
        };
        match result {
            Ok(()) => {
                let run_id_string = run_id.to_string();
                changed.push(run_id_string.clone());
                if !json {
                    fabro_util::printerr!(printer, "{}", short_run_id(&run_id_string));
                }
            }
            Err(err) => {
                if !json {
                    fabro_util::printerr!(printer, "error: {identifier}: {err}");
                }
                errors.push(serde_json::json!({
                    "identifier": identifier,
                    "error": err.to_string(),
                }));
                had_errors = true;
            }
        }
    }

    if json {
        let mut body = serde_json::Map::new();
        body.insert(action.json_key().to_string(), serde_json::json!(changed));
        body.insert("errors".to_string(), serde_json::json!(errors));
        print_json_pretty(&serde_json::Value::Object(body))?;
    }

    if had_errors {
        bail!("some runs could not be {}", action.past());
    }
    Ok(())
}
