use anyhow::Result;
use fabro_api::types::ForkRequest;
use fabro_util::terminal::Styles;

use super::checkpoints::{print_timeline, short_id, timeline_json};
use crate::args::ForkArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

/// Fork a run at a checkpoint into a new run, which starts at once.
pub(crate) async fn run(args: &ForkArgs, styles: &Styles, base_ctx: &CommandContext) -> Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.id;

    if args.list {
        let timeline = client.run_timeline(&run_id).await?;
        if ctx.json_output() {
            print_json_pretty(&timeline_json(&timeline))?;
        } else {
            print_timeline(&timeline, styles, printer);
        }
        return Ok(());
    }

    let response = client
        .fork_run(&run_id, ForkRequest {
            target: args.target.clone(),
        })
        .await?;

    if ctx.json_output() {
        print_json_pretty(&response)?;
    } else {
        fabro_util::printerr!(
            printer,
            "\nForked run {} -> {} at {} ({})",
            short_id(&response.source_run_id),
            short_id(&response.new_run_id),
            response.target,
            short_id(&response.checkpoint_sha)
        );
        fabro_util::printerr!(
            printer,
            "To follow: fabro attach {}",
            short_id(&response.new_run_id)
        );
    }

    Ok(())
}
