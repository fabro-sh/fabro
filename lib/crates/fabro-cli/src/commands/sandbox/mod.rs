use anyhow::Result;

use crate::args::{GlobalArgs, SandboxCommand};

pub(crate) async fn dispatch(command: SandboxCommand, globals: &GlobalArgs) -> Result<()> {
    match command {
        SandboxCommand::Cp(args) => super::run::cp::cp_command(args, globals).await,
        SandboxCommand::Preview(args) => super::run::preview::run(args, globals).await,
        SandboxCommand::Ssh(args) => super::run::ssh::run(args, globals).await,
    }
}
