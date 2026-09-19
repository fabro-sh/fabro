mod link;
mod unlink;

use anyhow::Result;

use super::{resolve_run_id, resolve_run_selector};
use crate::args::{ParentCommand, ParentNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: ParentNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        // The link command's future carries several client calls and sits
        // past clippy's stack budget; box it once at the call.
        ParentCommand::Link(args) => Box::pin(link::link_command(args, base_ctx)).await,
        ParentCommand::Unlink(args) => unlink::unlink_command(args, base_ctx).await,
    }
}
