use anyhow::Result;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, SandboxCommand};

pub(crate) async fn dispatch(
    command: SandboxCommand,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match command {
        SandboxCommand::Cp(args) => super::run::cp::cp_command(args, globals, printer).await,
        SandboxCommand::Preview(args) => super::run::preview::run(args, globals, printer).await,
        SandboxCommand::Ssh(args) => super::run::ssh::run(args, globals, printer).await,
    }
}
