mod list;
mod rm;
mod set;

use anyhow::Result;

use crate::args::{GlobalArgs, SecretCommand, SecretNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: SecretNamespace, globals: &GlobalArgs) -> Result<()> {
    let ctx = CommandContext::for_target(&ns.target)?;
    let server = ctx.server().await?;
    match ns.command {
        SecretCommand::List(args) => list::list_command(server.api(), &args, globals).await,
        SecretCommand::Rm(args) => rm::rm_command(server.api(), &args, globals).await,
        SecretCommand::Set(args) => set::set_command(server.api(), &args, globals).await,
    }
}
