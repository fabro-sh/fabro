use anyhow::{Result, bail};
use fabro_client::Client;
use fabro_types::{Run, RunId};
use fabro_util::terminal::Styles;
use futures::future::BoxFuture;

use crate::args::RunsCommands;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(crate) mod approval;
pub(crate) mod archive;
pub(crate) mod inspect;
pub(crate) mod list;
pub(crate) mod rm;

pub(crate) async fn dispatch(cmd: RunsCommands, base_ctx: &CommandContext) -> Result<()> {
    match cmd {
        RunsCommands::Ps(args) => {
            let styles = Styles::detect_stdout();
            list::list_command(&args, &styles, base_ctx).await
        }
        RunsCommands::Rm(args) => rm::remove_command(&args, base_ctx).await,
        RunsCommands::Inspect(args) => inspect::run(&args, base_ctx).await,
        RunsCommands::Approve(args) => approval::approve_command(&args, base_ctx).await,
        RunsCommands::Deny(args) => approval::deny_command(&args, base_ctx).await,
        RunsCommands::Archive(args) => archive::archive_command(&args, base_ctx).await,
        RunsCommands::Unarchive(args) => archive::unarchive_command(&args, base_ctx).await,
    }
}

pub(super) fn short_run_id(id: &str) -> &str {
    if id.len() > 12 { &id[..12] } else { id }
}

#[derive(Clone, Copy)]
pub(super) struct RunBatchAction {
    pub(super) past:     &'static str,
    pub(super) json_key: &'static str,
}

pub(super) async fn run_resolved_run_batch<F>(
    action: RunBatchAction,
    identifiers: &[String],
    ctx: &CommandContext,
    mut apply: F,
) -> Result<()>
where
    F: for<'a> FnMut(&'a Client, &'a RunId) -> BoxFuture<'a, Result<Run>>,
{
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
        match apply(client, &run_id).await {
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
        body.insert(action.json_key.to_string(), serde_json::json!(changed));
        body.insert("errors".to_string(), serde_json::json!(errors));
        print_json_pretty(&serde_json::Value::Object(body))?;
    }

    if had_errors {
        bail!("some runs could not be {}", action.past);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::args::parse_duration;
    use crate::shared::format_size;

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("24h").unwrap(), chrono::Duration::hours(24));
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration("7d").unwrap(), chrono::Duration::days(7));
    }

    #[test]
    fn parse_duration_rejects_invalid_unit() {
        let err = parse_duration("5m").unwrap_err();
        assert!(err.to_string().contains("invalid duration unit"));
    }

    #[test]
    fn format_size_humanizes_thresholds() {
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }
}
