use anyhow::Context;
use anyhow::Result;
use clap::Args;
use fabro_git_storage::gitobj::Store;
use fabro_util::terminal::Styles;
use git2::Repository;

#[derive(Debug, Args)]
pub struct ForkArgs {
    /// Run ID (or unambiguous prefix)
    pub run_id: String,

    /// Target checkpoint: node name, node@visit, or @ordinal (omit to fork from latest)
    pub target: Option<String>,

    /// Show the checkpoint timeline instead of forking
    #[arg(long)]
    pub list: bool,

    /// Skip pushing new branches to the remote
    #[arg(long)]
    pub no_push: bool,
}

pub fn run(args: &ForkArgs, styles: &Styles) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let run_id = fabro_workflows::operations::find_run_id_by_prefix(&repo, &args.run_id)?;
    let store = Store::new(repo);

    let timeline = fabro_workflows::operations::build_timeline(&store, &run_id)?;

    if args.list {
        let parallel_map = fabro_workflows::operations::load_parallel_map(&store, &run_id);
        super::rewind::print_timeline(&timeline, &parallel_map, styles);
        return Ok(());
    }

    let entry = if let Some(target_str) = &args.target {
        let target = fabro_workflows::operations::parse_target(target_str)?;
        let parallel_map = fabro_workflows::operations::load_parallel_map(&store, &run_id);
        fabro_workflows::operations::resolve_target(&timeline, &target, &parallel_map)?
    } else {
        timeline
            .last()
            .ok_or_else(|| anyhow::anyhow!("no checkpoints found for run {run_id}"))?
    };

    let new_run_id = fabro_workflows::operations::fork(&store, &run_id, entry, !args.no_push)?;

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
