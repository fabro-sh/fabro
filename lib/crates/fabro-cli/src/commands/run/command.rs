use anyhow::Result;
use fabro_config::ConfigLayer;
use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, RunArgs};

pub(crate) async fn execute(mut args: RunArgs, _globals: &GlobalArgs) -> Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli_settings = ConfigLayer::cli()?.resolve()?;
    args.verbose = args.verbose || cli_settings.verbose_enabled();

    let quiet = args.detach;
    let prevent_idle_sleep = cli_settings.prevent_idle_sleep_enabled();
    let (run_id, run_dir) = super::create::create_run(&args, styles, quiet)?;

    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(prevent_idle_sleep);

    #[cfg(not(feature = "sleep_inhibitor"))]
    let _ = prevent_idle_sleep;

    let child = super::start::start_run(&run_dir, false)?;

    if args.detach {
        println!("{run_id}");
    } else {
        let exit_code =
            super::attach::attach_run(&run_dir, Some(&run_id), true, styles, Some(child)).await?;
        super::output::print_run_summary(&run_dir, &run_id, styles);
        if exit_code != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
    }

    Ok(())
}
