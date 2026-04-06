use anyhow::Result;
use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, RunArgs};
use crate::server_client;
use crate::shared::print_json_pretty;
use crate::user_config::{self, settings_layer_with_storage_dir};

pub(crate) async fn execute(mut args: RunArgs, globals: &GlobalArgs) -> Result<()> {
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    let cli_settings = user_config::load_settings()?;
    let cli = settings_layer_with_storage_dir(None)?;
    args.verbose = args.verbose || cli_settings.verbose_enabled();

    let quiet = args.detach;
    let prevent_idle_sleep = cli_settings.prevent_idle_sleep_enabled();
    let created_run = Box::pin(super::create::create_run(&args, cli, styles, quiet)).await?;

    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(prevent_idle_sleep);

    #[cfg(not(feature = "sleep_inhibitor"))]
    let _ = prevent_idle_sleep;

    let client = server_client::connect_server_only(&args.target).await?;
    super::start::start_run_with_client(&client, &created_run.run_id, false).await?;

    if args.detach {
        if globals.json {
            print_json_pretty(&serde_json::json!({ "run_id": created_run.run_id }))?;
        } else {
            println!("{}", created_run.run_id);
        }
    } else {
        let exit_code = super::attach::attach_run_with_client(
            &client,
            &created_run.run_id,
            true,
            styles,
            globals.json,
        )
        .await?;
        if !globals.json {
            super::output::print_run_summary_with_client(
                &client,
                &created_run.run_id,
                created_run.local_run_dir.as_deref(),
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
