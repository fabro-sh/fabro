mod close;
mod create;
mod link;
mod merge;
mod unlink;
mod view;

use std::sync::Arc;

use anyhow::Result;
use fabro_client::Client;
use fabro_types::RunId;

use crate::args::{PrCommand, PrNamespace, ServerTargetArgs};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: PrNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        PrCommand::Create(args) => Box::pin(create::create_command(args, base_ctx)).await,
        PrCommand::Link(args) => link::link_command(args, base_ctx).await,
        PrCommand::Unlink(args) => unlink::unlink_command(args, base_ctx).await,
        PrCommand::View(args) => view::view_command(args, base_ctx).await,
        PrCommand::Merge(args) => merge::merge_command(args, base_ctx).await,
        PrCommand::Close(args) => close::close_command(args, base_ctx).await,
    }
}

async fn resolve_run_for_pr(
    base_ctx: &CommandContext,
    server: &ServerTargetArgs,
    selector: &str,
) -> Result<(CommandContext, Arc<Client>, RunId)> {
    let ctx = base_ctx.with_target(server)?;
    let client = ctx.server().await?;
    let run_id = match selector.parse::<RunId>() {
        Ok(run_id) => run_id,
        Err(_) => client.resolve_run(selector).await?.id,
    };
    Ok((ctx, client, run_id))
}
