use anyhow::Result;

use crate::args::{GlobalArgs, RunCommands};

pub(crate) mod attach;
pub(crate) mod command;
pub(crate) mod cp;
pub(crate) mod create;
pub(crate) mod detached;
pub(crate) mod diff;
pub(crate) mod fork;
pub(crate) mod launcher;
pub(crate) mod logs;
pub(crate) mod output;
pub(crate) mod overrides;
pub(crate) mod preview;
pub(crate) mod resume;
pub(crate) mod rewind;
pub(crate) mod run_progress;
pub(crate) mod ssh;
pub(crate) mod start;
pub(crate) mod wait;

pub async fn dispatch(cmd: RunCommands, globals: &GlobalArgs) -> Result<()> {
    match cmd {
        RunCommands::Run(args) => command::execute(args, globals).await,
        RunCommands::Create(args) => {
            let styles: &'static fabro_util::terminal::Styles =
                Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
            let cli_defaults = fabro_config::cli::load_cli_config(None)?;
            let (run_id, _run_dir) = create::create_run(&args, cli_defaults, styles, true).await?;
            println!("{run_id}");
            Ok(())
        }
        RunCommands::Start { run } => {
            let cli_config = crate::cli_config::load_cli_settings(None)?;
            let base = fabro_workflows::run_lookup::runs_base(&cli_config.storage_dir());
            let run_info = fabro_workflows::run_lookup::resolve_run(&base, &run)?;
            let child = start::start_run(&run_info.path, false)?;
            eprintln!("Started engine process (PID {})", child.id());
            Ok(())
        }
        RunCommands::Attach { run } => {
            let styles: &'static fabro_util::terminal::Styles =
                Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
            let cli_config = crate::cli_config::load_cli_settings(None)?;
            let base = fabro_workflows::run_lookup::runs_base(&cli_config.storage_dir());
            let run_info = fabro_workflows::run_lookup::resolve_run(&base, &run)?;
            let exit_code = attach::attach_run(&run_info.path, false, styles, None).await?;
            if exit_code != std::process::ExitCode::SUCCESS {
                std::process::exit(1);
            }
            Ok(())
        }
        RunCommands::Detached {
            run_dir,
            launcher_path,
            resume,
        } => detached::execute(run_dir, launcher_path, resume).await,
        RunCommands::Cp(args) => cp::cp_command(args).await,
        RunCommands::Preview(args) => preview::run(args).await,
        RunCommands::Ssh(args) => ssh::run(args).await,
        RunCommands::Diff(args) => diff::run(args).await,
        RunCommands::Logs(args) => {
            let styles = fabro_util::terminal::Styles::detect_stdout();
            logs::run(args, &styles)
        }
        RunCommands::Resume(args) => {
            let styles: &'static fabro_util::terminal::Styles =
                Box::leak(Box::new(fabro_util::terminal::Styles::detect_stderr()));
            #[cfg(feature = "sleep_inhibitor")]
            let _sleep_guard = {
                let cli_config = crate::cli_config::load_cli_settings(None)?;
                crate::sleep_inhibitor::guard(cli_config.prevent_idle_sleep_enabled())
            };
            resume::resume_command(args, styles).await
        }
        RunCommands::Rewind(args) => {
            let styles = fabro_util::terminal::Styles::detect_stderr();
            rewind::run(&args, &styles)
        }
        RunCommands::Fork(args) => {
            let styles = fabro_util::terminal::Styles::detect_stderr();
            fork::run(&args, &styles)
        }
        RunCommands::Wait(args) => {
            let styles = fabro_util::terminal::Styles::detect_stderr();
            wait::run(args, &styles)
        }
    }
}
