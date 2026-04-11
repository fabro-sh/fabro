mod list;
mod rm;
mod set;

use anyhow::Result;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, SecretCommand, SecretNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(
    ns: SecretNamespace,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&ns.target, printer)?;
    let server = ctx.server().await?;
    match ns.command {
        SecretCommand::List(args) => {
            list::list_command(server.api(), &args, globals, printer).await
        }
        SecretCommand::Rm(args) => rm::rm_command(server.api(), &args, globals, printer).await,
        SecretCommand::Set(args) => set::set_command(server.api(), &args, globals, printer).await,
    }
}
