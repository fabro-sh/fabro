use anyhow::Result;
use fabro_api::types::RewindRequest;
use fabro_util::terminal::Styles;

use super::checkpoints::{print_timeline, short_id, timeline_json};
use crate::args::RewindArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

/// Rewind a run to a checkpoint: a new run replaces it and starts at once.
pub(crate) async fn run(
    args: &RewindArgs,
    styles: &Styles,
    base_ctx: &CommandContext,
) -> Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.id;

    if args.list || args.target.is_none() {
        let timeline = client.run_timeline(&run_id).await?;
        if ctx.json_output() {
            print_json_pretty(&timeline_json(&timeline))?;
        } else {
            print_timeline(&timeline, styles, printer);
        }
        return Ok(());
    }

    let result = client
        .rewind_run(&run_id, RewindRequest {
            target: args.target.clone(),
        })
        .await?;
    let response = result.response;

    if ctx.json_output() {
        print_json_pretty(&serde_json::json!({
            "source_run_id": response.source_run_id,
            "new_run_id": response.new_run_id,
            "target": response.target,
            "checkpoint_sha": response.checkpoint_sha,
            "execution": response.execution,
            "firing": response.firing,
            "archived": response.archived,
            "archive_error": response.archive_error,
            "status": result.status,
        }))?;
    } else {
        fabro_util::printerr!(
            printer,
            "\nRewound {} to {}; new run {}",
            short_id(&response.source_run_id),
            response.target,
            short_id(&response.new_run_id)
        );
        fabro_util::printerr!(
            printer,
            "To follow: fabro attach {}",
            short_id(&response.new_run_id)
        );
        if !response.archived {
            let archive_error = response.archive_error.as_deref().unwrap_or("unknown error");
            fabro_util::printerr!(
                printer,
                "Warning: source not archived: {archive_error}. Run `fabro archive {}` to finish.",
                short_id(&response.source_run_id)
            );
        }
    }

    Ok(())
}
