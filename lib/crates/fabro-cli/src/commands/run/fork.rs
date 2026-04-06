use anyhow::Context;
use anyhow::Result;
use fabro_checkpoint::git::Store;
use fabro_util::terminal::Styles;
use fabro_workflow::operations::{ForkRunInput, RewindTarget, build_timeline_or_rebuild, fork};
use git2::Repository;

use crate::args::{ForkArgs, GlobalArgs};
use crate::commands::store::rebuild::rebuild_run_store;
use crate::server_runs::ServerRunLookup;
use crate::shared::print_json_pretty;
use crate::user_config::load_settings_with_storage_dir;

pub(crate) async fn run(args: &ForkArgs, styles: &Styles, globals: &GlobalArgs) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
    let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
    let run = lookup.resolve(&args.run_id)?;
    let run_id = run.run_id();
    let store = Store::new(repo);
    let events = lookup.client().list_run_events(&run_id, None, None).await?;
    let run_store = rebuild_run_store(&run_id, &events).await?;

    let timeline = build_timeline_or_rebuild(&store, Some(&run_store), &run_id).await?;

    if args.list {
        if globals.json {
            print_json_pretty(&super::rewind::timeline_entries_json(&timeline))?;
            return Ok(());
        }
        super::rewind::print_timeline(&timeline, styles);
        return Ok(());
    }

    let target = args
        .target
        .as_deref()
        .map(str::parse::<RewindTarget>)
        .transpose()?;
    let new_run_id = fork(
        &store,
        &ForkRunInput {
            source_run_id: run_id,
            target,
            push: !args.no_push,
        },
    )?;

    let run_id_string = run_id.to_string();
    let new_run_id_string = new_run_id.to_string();

    if globals.json {
        let target = args.target.clone().unwrap_or_else(|| "latest".to_string());
        print_json_pretty(&serde_json::json!({
            "source_run_id": run_id_string,
            "new_run_id": new_run_id_string,
            "target": target,
        }))?;
    } else {
        eprintln!(
            "\nForked run {} -> {}",
            &run_id_string[..8.min(run_id_string.len())],
            &new_run_id_string[..8.min(new_run_id_string.len())]
        );
        eprintln!(
            "To resume: fabro resume {}",
            &new_run_id_string[..8.min(new_run_id_string.len())]
        );
    }

    Ok(())
}
