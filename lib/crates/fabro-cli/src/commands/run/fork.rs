use anyhow::Context;
use anyhow::Result;
use fabro_config::FabroSettingsExt;
use fabro_git_storage::gitobj::Store;
use fabro_util::terminal::Styles;
use fabro_workflows::operations::{
    ForkRunInput, RewindTarget, build_timeline_or_rebuild, find_run_id_by_prefix_or_store, fork,
};
use git2::Repository;

use crate::args::ForkArgs;
use crate::cli_config::load_cli_settings;
use crate::store::{build_store, open_run_reader};

pub(crate) async fn run(args: &ForkArgs, styles: &Styles) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let cli_settings = load_cli_settings(None)?;
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
            source_run_id: run_id.clone(),
            target,
            push: !args.no_push,
        },
    )?;

    eprintln!(
        "\nForked run {} -> {}",
        &run_id[..8.min(run_id.len())],
        &new_run_id[..8.min(new_run_id.len())]
    );
    eprintln!(
        "To resume: fabro resume {}",
        &new_run_id[..8.min(new_run_id.len())]
    );

    Ok(())
}
