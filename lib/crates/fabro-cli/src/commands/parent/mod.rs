mod link;
mod unlink;

use std::sync::Arc;

use anyhow::Result;
use fabro_client::Client;
use fabro_types::RunId;

use crate::args::{ParentCommand, ParentNamespace, ServerTargetArgs};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: ParentNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        ParentCommand::Link(args) => link::link_command(args, base_ctx).await,
        ParentCommand::Unlink(args) => unlink::unlink_command(args, base_ctx).await,
    }
}

async fn resolve_run_selector(
    base_ctx: &CommandContext,
    server: &ServerTargetArgs,
    selector: &str,
) -> Result<(CommandContext, Arc<Client>, RunId)> {
    let ctx = base_ctx.with_target(server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(selector).await?.id;
    Ok((ctx, client, run_id))
}
