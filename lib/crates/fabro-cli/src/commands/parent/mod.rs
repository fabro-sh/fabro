mod link;
mod unlink;

use anyhow::Result;

use super::{resolve_run_id, resolve_run_selector};
use crate::args::{ParentCommand, ParentNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: ParentNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        ParentCommand::Link(args) => link::link_command(args, base_ctx).await,
        ParentCommand::Unlink(args) => unlink::unlink_command(args, base_ctx).await,
    }
}
