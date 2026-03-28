use anyhow::Result;
use fabro_config::FabroSettingsExt;
use fabro_config::cli::load_cli_config;
use fabro_util::terminal::Styles;
use fabro_workflows::run_lookup::{resolve_run, runs_base};

use crate::args::{GlobalArgs, RunCommands};
use crate::cli_config::load_cli_settings;

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

pub(super) fn short_run_id(id: &str) -> &str {
    if id.len() > 12 { &id[..12] } else { id }
}

pub(crate) async fn dispatch(cmd: RunCommands, globals: &GlobalArgs) -> Result<()> {
    match cmd {
        RunCommands::Run(args) => command::execute(args, globals).await,
        RunCommands::Create(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let cli_defaults = load_cli_config(None)?;
            let (run_id, _run_dir) = create::create_run(&args, cli_defaults, styles, true)?;
            println!("{run_id}");
            Ok(())
        }
        RunCommands::Start { run } => {
            let cli_settings = load_cli_settings(None)?;
            let base = runs_base(&cli_settings.storage_dir());
            let run_info = resolve_run(&base, &run)?;
            let child = start::start_run(&run_info.path, false)?;
            eprintln!("Started engine process (PID {})", child.id());
            Ok(())
        }
        RunCommands::Attach { run } => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            let cli_settings = load_cli_settings(None)?;
            let base = runs_base(&cli_settings.storage_dir());
            let run_info = resolve_run(&base, &run)?;
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
            let styles = Styles::detect_stdout();
            logs::run(&args, &styles)
        }
        RunCommands::Resume(args) => {
            let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
            #[cfg(feature = "sleep_inhibitor")]
            let _sleep_guard = {
                let cli_settings = load_cli_settings(None)?;
                crate::sleep_inhibitor::guard(cli_settings.prevent_idle_sleep_enabled())
            };
            resume::resume_command(args, styles).await
        }
        RunCommands::Rewind(args) => {
            let styles = Styles::detect_stderr();
            rewind::run(&args, &styles)
        }
        RunCommands::Fork(args) => {
            let styles = Styles::detect_stderr();
            fork::run(&args, &styles)
        }
        RunCommands::Wait(args) => {
            let styles = Styles::detect_stderr();
            wait::run(&args, &styles)
        }
    }
}
