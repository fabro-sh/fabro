use anyhow::Context;
use anyhow::Result;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table, print_stderr};
use fabro_config::FabroSettingsExt;
use fabro_git_storage::gitobj::Store;
use fabro_util::terminal::Styles;
use fabro_workflows::operations::{
    RewindInput, RewindTarget, RunTimeline, build_timeline_or_rebuild,
    find_run_id_by_prefix_or_store, rewind,
};
use git2::Repository;

use crate::args::{GlobalArgs, RewindArgs};
use crate::shared::color_if;
use crate::store::{build_store, open_run_reader};
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn run(args: &RewindArgs, styles: &Styles, globals: &GlobalArgs) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let cli_settings = load_user_settings_with_globals(globals)?;
    let durable_store = build_store(&cli_settings.storage_dir())?;
    let run_id =
        find_run_id_by_prefix_or_store(&repo, durable_store.as_ref(), &args.run_id).await?;
    let store = Store::new(repo);
    let run_store = open_run_reader(&cli_settings.storage_dir(), &run_id).await?;

    let timeline = build_timeline_or_rebuild(&store, run_store.as_deref(), &run_id).await?;

    if args.list || args.target.is_none() {
        print_timeline(&timeline, styles);
        return Ok(());
    }

    let target = args.target.as_deref().unwrap().parse::<RewindTarget>()?;

    rewind(
        &store,
        &RewindInput {
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

pub(crate) fn print_timeline(timeline: &RunTimeline, styles: &Styles) {
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
