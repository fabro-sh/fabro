mod close;
mod create;
mod list;
mod merge;
mod view;

use anyhow::Result;

use crate::args::{PrCommand, PrNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: PrNamespace, base_ctx: &CommandContext) -> Result<()> {
    match ns.command {
        PrCommand::Create(args) => Box::pin(create::create_command(args, base_ctx)).await,
        PrCommand::List(args) => list::list_command(args, base_ctx).await,
        PrCommand::View(args) => view::view_command(args, base_ctx).await,
        PrCommand::Merge(args) => merge::merge_command(args, base_ctx).await,
        PrCommand::Close(args) => close::close_command(args, base_ctx).await,
    }
}
