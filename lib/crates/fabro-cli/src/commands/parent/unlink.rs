use anyhow::Result;
use tracing::info;

use crate::args::ParentUnlinkArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn unlink_command(
    args: ParentUnlinkArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let (ctx, client, child_id) =
        super::resolve_run_selector(base_ctx, &args.server, &args.child_run).await?;
    let summary = client.unlink_run_parent(&child_id).await?;

    info!(%child_id, "Unlinked run parent");

    if ctx.json_output() {
        print_json_pretty(&summary)?;
    } else {
        fabro_util::printout!(ctx.printer(), "Unlinked parent: {}", child_id);
    }

    Ok(())
}
