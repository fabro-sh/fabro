mod list;
mod rm;
mod set;

use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;

use crate::args::{SecretCommand, SecretNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(
    ns: SecretNamespace,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&ns.target, printer, cli.clone(), cli_layer)?;
    let server = ctx.server().await?;
    match ns.command {
        SecretCommand::List(args) => list::list_command(&server, &args, cli, printer).await,
        SecretCommand::Rm(args) => rm::rm_command(&server, &args, cli, printer).await,
        SecretCommand::Set(args) => set::set_command(&server, &args, cli, printer).await,
    }
}
