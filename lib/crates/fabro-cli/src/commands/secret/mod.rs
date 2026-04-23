mod list;
mod rm;
mod set;

use anyhow::Result;

use crate::args::{SecretCommand, SecretNamespace};
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(ns: SecretNamespace, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&ns.target)?;
    let server = ctx.server().await?;
    let json_output =
        ctx.user_settings().cli.output.format == fabro_types::settings::cli::OutputFormat::Json;
    let printer = ctx.printer();
    match ns.command {
        SecretCommand::List(args) => list::list_command(&server, &args, json_output, printer).await,
        SecretCommand::Rm(args) => rm::rm_command(&server, &args, json_output, printer).await,
        SecretCommand::Set(args) => set::set_command(&server, &args, json_output, printer).await,
    }
}
