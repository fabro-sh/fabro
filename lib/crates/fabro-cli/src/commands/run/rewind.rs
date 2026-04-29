use anyhow::Result;
use fabro_api::types::RewindRequest;
use fabro_util::terminal::Styles;

use super::checkpoints::{ensure_origin_if_local, print_timeline, short_id, timeline_entries_json};
use crate::args::RewindArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(crate) async fn run(
    args: &RewindArgs,
    styles: &Styles,
    base_ctx: &CommandContext,
) -> Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    ensure_origin_if_local(client.as_ref(), &run_id, "rewind").await?;

    if args.list || args.target.is_none() {
        let timeline = client.run_timeline(&run_id).await?;
        if ctx.json_output() {
            print_json_pretty(&timeline_entries_json(&timeline))?;
            return Ok(());
        }
        print_timeline(&timeline_entries_json(&timeline), styles, printer);
        return Ok(());
    }

    let target = args
        .target
        .clone()
        .expect("rewind target should be present unless listing");
    let result = client
        .rewind_run(&run_id, RewindRequest {
            target: Some(target),
        })
        .await?;
    let response = result.response;

    if ctx.json_output() {
        print_json_pretty(&serde_json::json!({
            "source_run_id": response.source_run_id,
            "new_run_id": response.new_run_id,
            "target": response.target,
            "archived": response.archived,
            "archive_error": response.archive_error,
            "status": result.status,
        }))?;
    } else {
        fabro_util::printerr!(
            printer,
            "\nRewound {}; new run {}",
            short_id(&response.source_run_id),
            short_id(&response.new_run_id)
        );
        fabro_util::printerr!(
            printer,
            "To resume: fabro resume {}",
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
