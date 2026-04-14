use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;

use crate::args::SandboxCommand;

pub(crate) async fn dispatch(
    command: SandboxCommand,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    process_local_json: bool,
    printer: Printer,
) -> Result<()> {
    match command {
        SandboxCommand::Cp(args) => super::run::cp::cp_command(args, cli, cli_layer, printer).await,
        SandboxCommand::Preview(args) => {
            super::run::preview::run(args, cli, cli_layer, process_local_json, printer).await
        }
        SandboxCommand::Ssh(args) => {
            super::run::ssh::run(args, cli, cli_layer, process_local_json, printer).await
        }
    }
}
