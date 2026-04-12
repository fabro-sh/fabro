mod login;

use anyhow::Result;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, ProviderCommand, ProviderNamespace};

pub(crate) async fn dispatch(
    ns: ProviderNamespace,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        ProviderCommand::Login(args) => login::login_command(args, globals, printer).await,
    }
}
