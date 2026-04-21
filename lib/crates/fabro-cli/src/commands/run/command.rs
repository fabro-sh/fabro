use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat, OutputVerbosity};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::RunArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;
use crate::user_config::load_settings_with_storage_dir;

pub(crate) async fn execute(
    mut args: RunArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let ctx = CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer)?;
    let cli_defaults = load_settings_with_storage_dir(None)?;
    args.verbose = args.verbose || cli.output.verbosity == OutputVerbosity::Verbose;

    let quiet = args.detach;
    let prevent_idle_sleep = ctx.cli_settings().exec.prevent_idle_sleep;
    let created_run = Box::pin(super::create::create_run(
        &ctx,
        &args,
        cli_defaults,
        styles,
        quiet,
        printer,
    ))
    .await?;

    if !quiet {
        fabro_util::printerr!(
            printer,
            "    {} {}",
            styles.dim.apply_to("Run:"),
            styles.dim.apply_to(&created_run.run_id),
        );
    }

    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(prevent_idle_sleep);

    #[cfg(not(feature = "sleep_inhibitor"))]
    let _ = prevent_idle_sleep;

    let client = ctx.server().await?;
    super::start::start_run_with_client(&client, &created_run.run_id, false).await?;

    let json = cli.output.format == OutputFormat::Json;
    if args.detach {
        if json {
            print_json_pretty(&serde_json::json!({ "run_id": created_run.run_id }))?;
        } else {
            fabro_util::printout!(printer, "{}", created_run.run_id);
        }
    } else {
        let exit_code = Box::pin(super::attach::attach_run_with_client(
            &client,
            &created_run.run_id,
            true,
            styles,
            json,
            printer,
        ))
        .await?;
        if !json {
            super::output::print_run_summary_with_client(
                &client,
                &created_run.run_id,
                created_run.local_run_dir.as_deref(),
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
