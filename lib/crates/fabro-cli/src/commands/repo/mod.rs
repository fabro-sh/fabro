pub(crate) mod deinit;
pub(crate) mod init;

use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;

use crate::args::{RepoCommand, RepoNamespace};
use crate::shared::print_json_pretty;

pub(crate) async fn dispatch(
    ns: RepoNamespace,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    match ns.command {
        RepoCommand::Init(args) => {
            let created = init::run_init(&args, cli, cli_layer, printer).await?;
            if cli.output.format == OutputFormat::Json {
                print_json_pretty(&serde_json::json!({ "created": created }))?;
            }
            Ok(())
        }
        RepoCommand::Deinit => {
            let removed = deinit::run_deinit(cli, printer)?;
            if cli.output.format == OutputFormat::Json {
                print_json_pretty(&serde_json::json!({ "removed": removed }))?;
            }
            Ok(())
        }
    }
}
