use fabro_util::terminal::Styles;

use crate::args::{GlobalArgs, ResumeArgs};
use crate::server_runs::ServerRunLookup;
use crate::shared::print_json_pretty;
use crate::user_config::load_user_settings_with_storage_dir;

/// Resume an interrupted workflow run.
///
/// Looks up the run by ID prefix, validates a checkpoint exists, cleans stale
/// artifacts from the previous execution, then asks the server to resume it
/// (identical to `fabro run`'s create→start→attach flow).
pub(crate) async fn resume_command(
    args: ResumeArgs,
    styles: &'static Styles,
    globals: &GlobalArgs,
) -> anyhow::Result<()> {
    let cli_settings = load_user_settings_with_storage_dir(args.storage_dir.as_deref())?;
    let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
    let run = lookup.resolve(&args.run)?;
    let run_id = run.run_id();
    let run_dir = run.path;

    super::start::start_run(&run_id, &cli_settings.storage_dir(), true).await?;

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
