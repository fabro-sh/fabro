use anyhow::Result;
use fabro_util::terminal::Styles;

use crate::args::{AttachArgs, RunCommands, RunWorkerArgs, StartArgs};
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

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

pub(crate) async fn dispatch(cmd: RunCommands, base_ctx: &CommandContext) -> Result<()> {
    let printer = base_ctx.printer();

    match cmd {
        RunCommands::Run(args) => Box::pin(command::execute(args, base_ctx)).await,
        RunCommands::Create(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let ctx = base_ctx.with_target(&args.target)?;
            let created_run = Box::pin(create::create_run(&ctx, &args, styles, true)).await?;
            if ctx.json_output() {
                print_json_pretty(&serde_json::json!({ "run_id": created_run.run_id }))?;
            } else {
                fabro_util::printout!(printer, "{}", created_run.run_id);
            }
            Ok(())
        }
        RunCommands::Start(StartArgs { server, run }) => {
            let ctx = base_ctx.with_target(&server)?;
            let client = ctx.server().await?;
            let run_id = client.resolve_run(&run).await?.run_id;
            start::start_run_with_client(client.as_ref(), &run_id, false).await?;
            if ctx.json_output() {
                print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
            }
            Ok(())
        }
        RunCommands::Attach(AttachArgs { server, run }) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let ctx = base_ctx.with_target(&server)?;
            let client = ctx.server().await?;
            let run_id = client.resolve_run(&run).await?.run_id;
            let json = ctx.json_output();
            let exit_code = Box::pin(attach::attach_run_with_client(
                client.as_ref(),
                &run_id,
                false,
                styles,
                json,
                ctx.verbose(),
                printer,
            ))
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
        RunCommands::Diff(args) => diff::run(args, base_ctx).await,
        RunCommands::Logs(args) => {
            let styles = Styles::detect_stdout();
            logs::run(&args, &styles, base_ctx).await
        }
        RunCommands::Resume(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            #[cfg(feature = "sleep_inhibitor")]
            let _sleep_guard = {
                let ctx = base_ctx.with_target(&args.server)?;
                crate::sleep_inhibitor::guard(ctx.user_settings().cli.exec.prevent_idle_sleep)
            };
            Box::pin(resume::resume_command(args, styles, base_ctx)).await
        }
        RunCommands::Rewind(args) => {
            let styles = Styles::detect_stderr();
            Box::pin(rewind::run(&args, &styles, base_ctx)).await
        }
        RunCommands::Fork(args) => {
            let styles = Styles::detect_stderr();
            Box::pin(fork::run(&args, &styles, base_ctx)).await
        }
        RunCommands::Wait(args) => {
            let styles = Styles::detect_stderr();
            wait::run(&args, &styles, base_ctx).await
        }
    }
}
