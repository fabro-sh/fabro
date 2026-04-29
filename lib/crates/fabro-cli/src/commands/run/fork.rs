use anyhow::Result;
use fabro_api::types::ForkRequest;
use fabro_util::terminal::Styles;

use crate::args::ForkArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(crate) async fn run(args: &ForkArgs, styles: &Styles, base_ctx: &CommandContext) -> Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    super::checkpoints::ensure_origin_if_local(client.as_ref(), &run_id, "fork").await?;

    if args.list {
        let timeline = client.run_timeline(&run_id).await?;
        if ctx.json_output() {
            print_json_pretty(&super::checkpoints::timeline_entries_json(&timeline))?;
            return Ok(());
        }
        let entries = super::checkpoints::timeline_entries_json(&timeline);
        super::checkpoints::print_timeline(&entries, styles, printer);
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
            "\nForked run {} -> {}",
            super::checkpoints::short_id(&response.source_run_id),
            super::checkpoints::short_id(&response.new_run_id)
        );
        fabro_util::printerr!(
            printer,
            "To resume: fabro resume {}",
            super::checkpoints::short_id(&response.new_run_id)
        );
    }

    Ok(())
}
