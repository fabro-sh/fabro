use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::{AttachArgs, RunCommands, RunWorkerArgs, StartArgs};
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;
use crate::user_config::load_settings_with_storage_dir;

pub(crate) mod attach;
pub(crate) mod command;
pub(crate) mod cp;
pub(crate) mod create;
pub(crate) mod diff;
pub(crate) mod fork;
pub(crate) mod logs;
pub(crate) mod output;
pub(crate) mod overrides;
pub(crate) mod preview;
pub(crate) mod resume;
pub(crate) mod rewind;
pub(crate) mod run_progress;
pub(crate) mod runner;
pub(crate) mod ssh;
pub(crate) mod start;
pub(crate) mod wait;

pub(crate) async fn dispatch(
    cmd: RunCommands,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    _process_local_json: bool,
    printer: Printer,
) -> Result<()> {
    match cmd {
        RunCommands::Run(args) => Box::pin(command::execute(args, cli, cli_layer, printer)).await,
        RunCommands::Create(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let cli_defaults = load_settings_with_storage_dir(None)?;
            let ctx = CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer)?;
            let created_run = Box::pin(create::create_run(
                &ctx,
                &args,
                cli_defaults,
                styles,
                true,
                printer,
            ))
            .await?;
            if cli.output.format == OutputFormat::Json {
                print_json_pretty(&serde_json::json!({ "run_id": created_run.run_id }))?;
            } else {
                fabro_util::printout!(printer, "{}", created_run.run_id);
            }
            Ok(())
        }
        RunCommands::Start(StartArgs { server, run }) => {
            let ctx = CommandContext::for_target(&server, printer, cli.clone(), cli_layer)?;
            let client = ctx.server().await?;
            let run_id = client.resolve_run(&run).await?.run_id;
            start::start_run_with_client(client.as_ref(), &run_id, false).await?;
            if cli.output.format == OutputFormat::Json {
                print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
            }
            Ok(())
        }
        RunCommands::Attach(AttachArgs { server, run }) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let ctx = CommandContext::for_target(&server, printer, cli.clone(), cli_layer)?;
            let client = ctx.server().await?;
            let run_id = client.resolve_run(&run).await?.run_id;
            let exit_code = attach::attach_run_with_client(
                client.as_ref(),
                &run_id,
                false,
                styles,
                cli.output.format == OutputFormat::Json,
                printer,
            )
            .await?;
            if exit_code != std::process::ExitCode::SUCCESS {
                std::process::exit(1);
            }
            Ok(())
        }
        RunCommands::RunWorker(RunWorkerArgs {
            server,
            storage_dir,
            artifact_upload_token,
            run_dir,
            run_id,
            mode,
        }) => {
            runner::execute(
                run_id,
                server,
                storage_dir,
                artifact_upload_token,
                run_dir,
                mode,
            )
            .await
        }
        RunCommands::Diff(args) => diff::run(args, cli, cli_layer, printer).await,
        RunCommands::Logs(args) => {
            let styles = Styles::detect_stdout();
            logs::run(&args, &styles, cli, cli_layer, printer).await
        }
        RunCommands::Resume(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            #[cfg(feature = "sleep_inhibitor")]
            let _sleep_guard = {
                let ctx =
                    CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
                crate::sleep_inhibitor::guard(ctx.cli_settings().exec.prevent_idle_sleep)
            };
            resume::resume_command(args, styles, cli, cli_layer, printer).await
        }
        RunCommands::Rewind(args) => {
            let styles = Styles::detect_stderr();
            Box::pin(rewind::run(&args, &styles, cli, cli_layer, printer)).await
        }
        RunCommands::Fork(args) => {
            let styles = Styles::detect_stderr();
            Box::pin(fork::run(&args, &styles, cli, cli_layer, printer)).await
        }
        RunCommands::Wait(args) => {
            let styles = Styles::detect_stderr();
            wait::run(&args, &styles, cli, cli_layer, printer).await
        }
    }
}
