use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::ResumeArgs;
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::print_json_pretty;

/// Resume an interrupted workflow run.
///
/// Looks up the run by ID prefix, validates a checkpoint exists, cleans stale
/// artifacts from the previous execution, then asks the server to resume it
/// (identical to `fabro run`'s create→start→attach flow).
pub(crate) async fn resume_command(
    args: ResumeArgs,
    styles: &'static Styles,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> anyhow::Result<()> {
    let ctx = CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    let run = lookup.resolve(&args.run)?;
    let run_id = run.run_id();

    super::start::start_run_with_client(lookup.client(), &run_id, true).await?;

    let json = cli.output.format == OutputFormat::Json;
    if args.detach {
        if json {
            print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
        } else {
            fabro_util::printout!(printer, "{run_id}");
        }
    } else {
        let exit_code = super::attach::attach_run_with_client(
            lookup.client(),
            &run_id,
            true,
            styles,
            json,
            printer,
        )
        .await?;
        if !json {
            super::output::print_run_summary_with_client(
                lookup.client(),
                &run_id,
                None,
                styles,
                printer,
            )
            .await?;
        }
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }
    Ok(())
}
