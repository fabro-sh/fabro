mod login;

use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;

use crate::args::{ProviderCommand, ProviderNamespace};

pub(crate) async fn dispatch(
    ns: ProviderNamespace,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    process_local_json: bool,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        ProviderCommand::Login(args) => {
            login::login_command(args, cli, cli_layer, process_local_json, printer).await
        }
    }
}
