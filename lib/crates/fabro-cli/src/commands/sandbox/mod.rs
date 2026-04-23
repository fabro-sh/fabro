use anyhow::Result;

use crate::args::SandboxCommand;
use crate::command_context::CommandContext;

pub(crate) async fn dispatch(command: SandboxCommand, base_ctx: &CommandContext) -> Result<()> {
    match command {
        SandboxCommand::Cp(args) => super::run::cp::cp_command(args, base_ctx).await,
        SandboxCommand::Preview(args) => super::run::preview::run(args, base_ctx).await,
        SandboxCommand::Ssh(args) => super::run::ssh::run(args, base_ctx).await,
    }
}
