use anyhow::Result;
use fabro_util::terminal::Styles;

use super::checkpoints::{print_timeline, timeline_json};
use crate::args::TimelineArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

/// Show the checkpoint timeline of a run.
pub(crate) async fn run(
    args: &TimelineArgs,
    styles: &Styles,
    base_ctx: &CommandContext,
) -> Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.id;
    let timeline = client.run_timeline(&run_id).await?;
    if ctx.json_output() {
        print_json_pretty(&timeline_json(&timeline))?;
    } else {
        print_timeline(&timeline, styles, printer);
    }
    Ok(())
}
