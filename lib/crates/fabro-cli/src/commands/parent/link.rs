use anyhow::Result;
use tracing::info;

use crate::args::ParentLinkArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(super) async fn link_command(args: ParentLinkArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let (child_id, parent_id) = tokio::try_join!(
        super::resolve_run_id(client.as_ref(), &args.child_run),
        super::resolve_run_id(client.as_ref(), &args.parent_run),
    )?;
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
