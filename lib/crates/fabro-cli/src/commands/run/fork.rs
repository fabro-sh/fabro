use anyhow::Context;
use anyhow::Result;
use fabro_checkpoint::git::Store;
use fabro_config::FabroSettingsExt;
use fabro_util::terminal::Styles;
use fabro_workflow::operations::{
    ForkRunInput, RewindTarget, build_timeline_or_rebuild, find_run_id_by_prefix_or_store, fork,
};
use git2::Repository;

use crate::args::{ForkArgs, GlobalArgs};
use crate::store::{build_store, open_run_reader};
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn run(args: &ForkArgs, styles: &Styles, globals: &GlobalArgs) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let cli_settings = load_user_settings_with_globals(globals)?;
    let durable_store = build_store(&cli_settings.storage_dir())?;
    let run_id =
        find_run_id_by_prefix_or_store(&repo, durable_store.as_ref(), &args.run_id).await?;
    let store = Store::new(repo);
    let run_store = open_run_reader(&cli_settings.storage_dir(), &run_id).await?;

    let timeline = build_timeline_or_rebuild(&store, run_store.as_deref(), &run_id).await?;

    if args.list {
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

    eprintln!(
        "\nForked run {} -> {}",
        &run_id_string[..8.min(run_id_string.len())],
        &new_run_id_string[..8.min(new_run_id_string.len())]
    );
    eprintln!(
        "To resume: fabro resume {}",
        &new_run_id_string[..8.min(new_run_id_string.len())]
    );

    Ok(())
}
