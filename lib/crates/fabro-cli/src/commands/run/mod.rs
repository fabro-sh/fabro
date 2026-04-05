use anyhow::Result;
use fabro_util::terminal::Styles;

use crate::args::{AttachArgs, GlobalArgs, RunArgs, RunCommands, RunnerArgs, StartArgs};
use crate::server_runs::ServerRunLookup;
use crate::shared::print_json_pretty;
use crate::user_config::{load_user_settings_with_storage_dir, user_layer_with_storage_dir};

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

pub(crate) async fn dispatch(cmd: RunCommands, globals: &GlobalArgs) -> Result<()> {
    match cmd {
        RunCommands::Run(mut args) => {
            apply_json_defaults(&mut args, globals);
            Box::pin(command::execute(args, globals)).await
        }
        RunCommands::Create(mut args) => {
            apply_json_defaults(&mut args, globals);
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let cli = user_layer_with_storage_dir(args.storage_dir.as_deref())?;
            let (run_id, _run_dir) = Box::pin(create::create_run(&args, cli, styles, true)).await?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
            } else {
                println!("{run_id}");
            }
            Ok(())
        }
        RunCommands::Start(StartArgs { storage_dir, run }) => {
            let cli_settings = load_user_settings_with_storage_dir(storage_dir.as_deref())?;
            let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
            let run_info = lookup.resolve(&run)?;
            let run_id = run_info.run_id();
            start::start_run(&run_id, &cli_settings.storage_dir(), false).await?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
            }
            Ok(())
        }
        RunCommands::Attach(AttachArgs { storage_dir, run }) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let cli_settings = load_user_settings_with_storage_dir(storage_dir.as_deref())?;
            let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
            let run_info = lookup.resolve(&run)?;
            let run_id = run_info.run_id();
            let exit_code = attach::attach_run(
                &run_info.path,
                Some(cli_settings.storage_dir().as_path()),
                Some(&run_id),
                false,
                styles,
                globals.json,
            )
            .await?;
            if exit_code != std::process::ExitCode::SUCCESS {
                std::process::exit(1);
            }
            Ok(())
        }
        RunCommands::Runner(RunnerArgs {
            storage_dir,
            run_id,
            resume,
        }) => runner::execute(run_id, storage_dir.clone_path(), resume).await,
        RunCommands::Diff(args) => diff::run(args, globals).await,
        RunCommands::Logs(args) => {
            let styles = Styles::detect_stdout();
            logs::run(&args, &styles, globals).await
        }
        RunCommands::Resume(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            #[cfg(feature = "sleep_inhibitor")]
            let _sleep_guard = {
                let cli_settings =
                    load_user_settings_with_storage_dir(args.storage_dir.as_deref())?;
                crate::sleep_inhibitor::guard(cli_settings.prevent_idle_sleep_enabled())
            };
            resume::resume_command(args, styles, globals).await
        }
        RunCommands::Rewind(args) => {
            let styles = Styles::detect_stderr();
            Box::pin(rewind::run(&args, &styles, globals)).await
        }
        RunCommands::Fork(args) => {
            let styles = Styles::detect_stderr();
            fork::run(&args, &styles, globals).await
        }
        RunCommands::Wait(args) => {
            let styles = Styles::detect_stderr();
            wait::run(&args, &styles, globals).await
        }
    }
}
