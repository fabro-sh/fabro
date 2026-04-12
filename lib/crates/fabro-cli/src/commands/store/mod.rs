pub(crate) mod dump;
pub(crate) mod rebuild;

use anyhow::Result;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, StoreCommand, StoreNamespace};

pub(crate) async fn dispatch(
    ns: StoreNamespace,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        StoreCommand::Dump(args) => dump::dump_command(&args, globals, printer).await,
    }
}
