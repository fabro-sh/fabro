use anyhow::Result;
use fabro_config::FabroSettingsExt;
use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, RunArgs};
use crate::shared::print_json_pretty;
use crate::user_config::{self, user_layer_with_globals};

pub(crate) async fn execute(mut args: RunArgs, globals: &GlobalArgs) -> Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli_settings = user_config::load_user_settings_with_globals(globals)?;
    let cli = user_layer_with_globals(globals)?;
    args.verbose = args.verbose || cli_settings.verbose_enabled();

    let quiet = args.detach;
    let prevent_idle_sleep = cli_settings.prevent_idle_sleep_enabled();
    let (run_id, run_dir) = super::create::create_run(&args, cli, styles, quiet)?;

    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(prevent_idle_sleep);

    #[cfg(not(feature = "sleep_inhibitor"))]
    let _ = prevent_idle_sleep;

    let child =
        super::start::start_run(&run_dir, &run_id, &cli_settings.storage_dir(), false).await?;

    if args.detach {
        if globals.json {
            print_json_pretty(&serde_json::json!({ "run_id": run_id }))?;
        } else {
            println!("{run_id}");
        }
    } else {
        let exit_code = super::attach::attach_run(
            &run_dir,
            Some(cli_settings.storage_dir().as_path()),
            Some(&run_id),
            true,
            styles,
            Some(child),
            globals.json,
        )
        .await?;
        if !globals.json {
            super::output::print_run_summary(
                cli_settings.storage_dir().as_path(),
                &run_dir,
                run_id,
                styles,
            )
            .await?;
        }
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }

    Ok(())
}
