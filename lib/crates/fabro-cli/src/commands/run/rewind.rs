use anyhow::Context;
use anyhow::Result;
use cli_table::format::{Border, Separator};
use cli_table::{print_stderr, Cell, CellStruct, Color, Style, Table};
use fabro_git_storage::gitobj::Store;
use fabro_util::terminal::Styles;
use git2::Repository;

use crate::args::RewindArgs;
use crate::shared::color_if;

pub fn run(args: &RewindArgs, styles: &Styles) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let run_id = fabro_workflows::operations::find_run_id_by_prefix(&repo, &args.run_id)?;
    let store = Store::new(repo);

    let timeline = fabro_workflows::operations::build_timeline(&store, &run_id)?;

    if args.list || args.target.is_none() {
        print_timeline(&timeline, styles);
        return Ok(());
    }

    let target = args
        .target
        .as_deref()
        .unwrap()
        .parse::<fabro_workflows::operations::RewindTarget>()?;

    fabro_workflows::operations::rewind(
        &store,
        fabro_workflows::operations::RewindInput {
            run_id: run_id.clone(),
            target,
            push: !args.no_push,
        },
    )?;

    eprintln!(
        "\nTo resume: fabro resume {}",
        &run_id[..8.min(run_id.len())]
    );

    Ok(())
}

pub(crate) fn print_timeline(timeline: &fabro_workflows::operations::RunTimeline, styles: &Styles) {
    if timeline.entries.is_empty() {
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
        .entries
        .iter()
        .map(|entry| {
            let ordinal_str = format!("@{}", entry.ordinal);
            let mut details = Vec::new();
            if entry.visit > 1 {
                details.push(format!("visit {}, loop", entry.visit));
            }
            if timeline.parallel_map.contains_key(&entry.node_name) {
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
