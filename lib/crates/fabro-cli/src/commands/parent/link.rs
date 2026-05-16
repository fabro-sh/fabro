use anyhow::Result;
use tracing::info;

use crate::args::ParentLinkArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn link_command(args: ParentLinkArgs, base_ctx: &CommandContext) -> Result<()> {
    let (ctx, client, child_id) =
        super::resolve_run_selector(base_ctx, &args.server, &args.child_run).await?;
    let parent_id = client.resolve_run(&args.parent_run).await?.id;
    let summary = client.link_run_parent(&child_id, &parent_id).await?;

    info!(%child_id, %parent_id, "Linked run parent");

    if ctx.json_output() {
        print_json_pretty(&summary)?;
    } else {
        fabro_util::printout!(
            ctx.printer(),
            "Linked parent: {} -> {}",
            child_id,
            parent_id
        );
    }

    Ok(())
}
