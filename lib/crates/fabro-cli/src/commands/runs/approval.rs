use anyhow::{Result, bail};

use super::short_run_id;
use crate::args::{RunsApproveArgs, RunsDenyArgs};
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(crate) async fn approve_command(
    args: &RunsApproveArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    run_bulk(Action::Approve, &args.runs, None, &ctx).await
}

pub(crate) async fn deny_command(args: &RunsDenyArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    run_bulk(Action::Deny, &args.runs, args.reason.clone(), &ctx).await
}

#[derive(Clone, Copy)]
enum Action {
    Approve,
    Deny,
}

impl Action {
    fn past(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Deny => "denied",
        }
    }
}

async fn run_bulk(
    action: Action,
    identifiers: &[String],
    reason: Option<String>,
    ctx: &CommandContext,
) -> Result<()> {
    let client = ctx.server().await?;
    let client = client.as_ref();
    let json = ctx.json_output();
    let printer = ctx.printer();
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

        let run_id = run.id;
        let result = match action {
            Action::Approve => client.approve_run(&run_id).await,
            Action::Deny => client.deny_run(&run_id, reason.clone()).await,
        };
        match result {
            Ok(_) => {
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
        body.insert(action.past().to_string(), serde_json::json!(changed));
        body.insert("errors".to_string(), serde_json::json!(errors));
        print_json_pretty(&serde_json::Value::Object(body))?;
    }

    if had_errors {
        bail!("some runs could not be {}", action.past());
    }
    Ok(())
}
