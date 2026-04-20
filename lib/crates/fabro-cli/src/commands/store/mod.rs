pub(crate) mod dump;
pub(crate) mod rebuild;
mod run_export;

use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;

use crate::args::{StoreCommand, StoreNamespace};

pub(crate) async fn dispatch(
    ns: StoreNamespace,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        StoreCommand::Dump(args) => dump::dump_command(&args, cli, cli_layer, printer).await,
    }
}
