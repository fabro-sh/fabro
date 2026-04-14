pub(crate) mod dump;
pub(crate) mod rebuild;

use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_util::printer::Printer;

use crate::args::{StoreCommand, StoreNamespace};

pub(crate) async fn dispatch(
    ns: StoreNamespace,
    cli: &CliSettings,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        StoreCommand::Dump(args) => dump::dump_command(&args, cli, printer).await,
    }
}
