use anyhow::Context;
use anyhow::Result;
use clap::Args;
use cli_table::format::{Border, Separator};
use cli_table::{print_stderr, Cell, CellStruct, Color, Style, Table};
use fabro_git_storage::gitobj::Store;
use fabro_util::terminal::Styles;
use git2::Repository;

use super::shared::color_if;

#[derive(Debug, Args)]
pub struct RewindArgs {
    /// Run ID (or unambiguous prefix)
    pub run_id: String,

    /// Target checkpoint: node name, node@visit, or @ordinal (omit with --list)
    pub target: Option<String>,

    /// Show the checkpoint timeline instead of rewinding
    #[arg(long)]
    pub list: bool,

    /// Skip force-pushing rewound refs to the remote
    #[arg(long)]
    pub no_push: bool,
}

pub fn run(args: &RewindArgs, styles: &Styles) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let run_id = fabro_workflows::operations::find_run_id_by_prefix(&repo, &args.run_id)?;
    let store = Store::new(repo);

    let timeline = fabro_workflows::operations::build_timeline(&store, &run_id)?;

    if args.list || args.target.is_none() {
        let parallel_map = fabro_workflows::operations::load_parallel_map(&store, &run_id);
        print_timeline(&timeline, &parallel_map, styles);
        return Ok(());
    }

    let target = fabro_workflows::operations::parse_target(args.target.as_deref().unwrap())?;
    let parallel_map = fabro_workflows::operations::load_parallel_map(&store, &run_id);
    let entry = fabro_workflows::operations::resolve_target(&timeline, &target, &parallel_map)?;

    fabro_workflows::operations::rewind(&store, &run_id, entry, !args.no_push)?;

    eprintln!(
        "\nTo resume: fabro resume {}",
        &run_id[..8.min(run_id.len())]
    );

    Ok(())
}

pub(crate) fn print_timeline(
    timeline: &[fabro_workflows::operations::TimelineEntry],
    parallel_map: &std::collections::HashMap<String, String>,
    styles: &Styles,
) {
    if timeline.is_empty() {
        eprintln!("No checkpoints found.");
        return;
    }

    let use_color = styles.use_color;
    let title = vec![
        "@".cell().bold(true),
        "Node".cell().bold(true),
        "Details".cell().bold(true),
    ];

    let rows: Vec<Vec<CellStruct>> = timeline
        .iter()
        .map(|entry| {
            let ordinal_str = format!("@{}", entry.ordinal);
            let mut details = Vec::new();
            if entry.visit > 1 {
                details.push(format!("visit {}, loop", entry.visit));
            }
            if parallel_map.contains_key(&entry.node_name) {
                details.push("parallel interior".to_string());
            }
            if entry.run_commit_sha.is_none() {
                details.push("no run commit".to_string());
            }

            let detail_str = if details.is_empty() {
                String::new()
            } else {
                format!("({})", details.join(", "))
            };

            vec![
                ordinal_str
                    .cell()
                    .foreground_color(color_if(use_color, Color::Cyan)),
                entry.node_name.clone().cell(),
                detail_str
                    .cell()
                    .foreground_color(color_if(use_color, Color::Ansi256(8))),
            ]
        })
        .collect();

    let table = rows
        .table()
        .title(title)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    let _ = print_stderr(table);
}
