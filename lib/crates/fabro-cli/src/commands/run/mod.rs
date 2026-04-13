use anyhow::Result;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::{AttachArgs, GlobalArgs, RunArgs, RunCommands, RunWorkerArgs, StartArgs};
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::print_json_pretty;
use crate::user_config::settings_layer_with_storage_dir;

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

fn apply_json_defaults(args: &mut RunArgs, globals: &GlobalArgs) {
    if globals.json {
        args.auto_approve = true;
    }
}

pub(crate) async fn dispatch(
    cmd: RunCommands,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    match cmd {
        RunCommands::Run(mut args) => {
            apply_json_defaults(&mut args, globals);
            Box::pin(command::execute(args, globals, printer)).await
        }
        RunCommands::Create(mut args) => {
            apply_json_defaults(&mut args, globals);
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let cli = settings_layer_with_storage_dir(None)?;
            let ctx = CommandContext::for_target(&args.target, printer)?;
            let created_run =
                Box::pin(create::create_run(&ctx, &args, cli, styles, true, printer)).await?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "run_id": created_run.run_id }))?;
            } else {
                fabro_util::printout!(printer, "{}", created_run.run_id);
            }
            Ok(())
        }
        RunCommands::Start(StartArgs { server, run }) => {
            let ctx = CommandContext::for_target(&server, printer)?;
            let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
            let run_info = lookup.resolve(&run)?;
            let run_id = run_info.run_id();
            start::start_run_with_client(lookup.client(), &run_id, false).await?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
            }
            Ok(())
        }
        RunCommands::Attach(AttachArgs { server, run }) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let ctx = CommandContext::for_target(&server, printer)?;
            let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
            let run_info = lookup.resolve(&run)?;
            let run_id = run_info.run_id();
            let exit_code = attach::attach_run_with_client(
                lookup.client(),
                &run_id,
                false,
                styles,
                globals.json,
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
        RunCommands::Diff(args) => diff::run(args, globals, printer).await,
        RunCommands::Logs(args) => {
            let styles = Styles::detect_stdout();
            logs::run(&args, &styles, globals, printer).await
        }
        RunCommands::Resume(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            #[cfg(feature = "sleep_inhibitor")]
            let _sleep_guard = {
                let ctx = CommandContext::for_target(&args.server, printer)?;
                crate::sleep_inhibitor::guard(ctx.cli_settings().exec.prevent_idle_sleep)
            };
            resume::resume_command(args, styles, globals, printer).await
        }
        RunCommands::Rewind(args) => {
            let styles = Styles::detect_stderr();
            Box::pin(rewind::run(&args, &styles, globals, printer)).await
        }
        RunCommands::Fork(args) => {
            let styles = Styles::detect_stderr();
            Box::pin(fork::run(&args, &styles, globals, printer)).await
        }
        RunCommands::Wait(args) => {
            let styles = Styles::detect_stderr();
            wait::run(&args, &styles, globals, printer).await
        }
    }
}
