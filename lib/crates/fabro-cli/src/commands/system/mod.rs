mod df;
mod events;
mod info;
mod prune;

use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;
pub(crate) use prune::parse_duration;

use crate::args::{SystemCommand, SystemNamespace};

pub(crate) async fn dispatch(
    ns: SystemNamespace,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        SystemCommand::Info(args) => info::info_command(&args, cli, cli_layer, printer).await,
        SystemCommand::Prune(args) => prune::prune_command(&args, cli, cli_layer, printer).await,
        SystemCommand::Df(args) => df::df_command(&args, cli, cli_layer, printer).await,
        SystemCommand::Events(args) => events::events_command(&args, cli, cli_layer, printer).await,
    }
}
