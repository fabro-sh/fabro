use fabro_types::settings::cli::{OutputFormat, OutputVerbosity};
use fabro_util::terminal::Styles;

use crate::args::ResumeArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

/// Resume an interrupted workflow run.
///
/// Looks up the run by ID prefix, validates a checkpoint exists, cleans stale
/// artifacts from the previous execution, then asks the server to resume it
/// (identical to `fabro run`'s create→start→attach flow).
pub(crate) async fn resume_command(
    args: ResumeArgs,
    styles: &'static Styles,
    base_ctx: &CommandContext,
) -> anyhow::Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.run_id;

    super::start::start_run_with_client(client.as_ref(), &run_id, true).await?;

    let json = ctx.user_settings().cli.output.format == OutputFormat::Json;
    if args.detach {
        if json {
            print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
        } else {
            fabro_util::printout!(printer, "{run_id}");
        }
    } else {
        let exit_code = Box::pin(super::attach::attach_run_with_client(
            client.as_ref(),
            &run_id,
            true,
            styles,
            json,
            ctx.user_settings().cli.output.verbosity == OutputVerbosity::Verbose,
            printer,
        ))
        .await?;
        if !json {
            super::output::print_run_summary_with_client(client.as_ref(), &run_id, styles, printer)
                .await?;
        }
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }
    Ok(())
}
