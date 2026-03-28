use anyhow::Context;
use anyhow::Result;
use fabro_git_storage::gitobj::Store;
use fabro_util::terminal::Styles;
use fabro_workflows::operations::{
    ForkRunInput, RewindTarget, build_timeline, find_run_id_by_prefix, fork,
};
use git2::Repository;

use crate::args::ForkArgs;

pub(crate) fn run(args: &ForkArgs, styles: &Styles) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let run_id = find_run_id_by_prefix(&repo, &args.run_id)?;
    let store = Store::new(repo);

    let timeline = build_timeline(&store, &run_id)?;

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
        ForkRunInput {
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
