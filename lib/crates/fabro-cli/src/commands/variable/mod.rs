mod get;
mod list;
mod rm;
mod set;

use anyhow::Result;

use crate::args::{VariableCommand, VariableNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: VariableNamespace, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&ns.target)?;
    match ns.command {
        VariableCommand::List(args) => list::list_command(&args, &ctx).await,
        VariableCommand::Get(args) => get::get_command(&args, &ctx).await,
        VariableCommand::Rm(args) => rm::rm_command(&args, &ctx).await,
        VariableCommand::Set(args) => set::set_command(&args, &ctx).await,
    }
}
