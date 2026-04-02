use anyhow::Context;
use anyhow::Result;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_checkpoint::git::Store;
use fabro_config::FabroSettingsExt;
use fabro_util::terminal::Styles;
use fabro_workflow::git::MetadataStore;
use fabro_workflow::operations::{
    RewindInput, RewindTarget, RunTimeline, build_timeline_or_rebuild,
    find_run_id_by_prefix_or_store, rewind,
};
use fabro_workflow::records::CheckpointExt;
use fabro_workflow::records::{RunRecord, RunRecordExt, StartRecord, StartRecordExt};
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
use fabro_workflow::run_status::{self, RunStatus};
use git2::Repository;
use serde::Serialize;

use crate::args::{GlobalArgs, RewindArgs};
use crate::shared::{color_if, print_json_pretty};
use crate::store::{build_store, open_run_reader};
use crate::user_config::load_user_settings_with_globals;

#[derive(Serialize)]
pub(crate) struct TimelineEntryJson {
    ordinal: usize,
    node_name: String,
    visit: usize,
    run_commit_sha: Option<String>,
}

pub(crate) async fn run(args: &RewindArgs, styles: &Styles, globals: &GlobalArgs) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let cli_settings = load_user_settings_with_globals(globals)?;
    let durable_store = build_store(&cli_settings.storage_dir())?;
    let run_id =
        find_run_id_by_prefix_or_store(&repo, durable_store.as_ref(), &args.run_id).await?;
    let store = Store::new(repo);
    let run_store = open_run_reader(&cli_settings.storage_dir(), &run_id).await?;
    let run_info = resolve_run_combined(
        durable_store.as_ref(),
        &runs_base(&cli_settings.storage_dir()),
        &run_id.to_string(),
    )
    .await
    .ok();

    let timeline = build_timeline_or_rebuild(&store, run_store.as_deref(), &run_id).await?;

    if args.list || args.target.is_none() {
        if globals.json {
            print_json_pretty(&timeline_entries_json(&timeline))?;
            return Ok(());
        }
        print_timeline(&timeline, styles);
        return Ok(());
    }

    let target = args.target.as_deref().unwrap().parse::<RewindTarget>()?;

    rewind(
        &store,
        &RewindInput {
            run_id,
            target,
            push: !args.no_push,
        },
    )?;
    if let Some(run_info) = run_info.as_ref() {
        reset_rewound_run_state(&store, durable_store.as_ref(), &run_id, &run_info.path).await?;
    }

    let run_id_string = run_id.to_string();

    if globals.json {
        print_json_pretty(&serde_json::json!({
            "run_id": run_id_string,
            "target": args.target.as_deref().unwrap(),
        }))?;
    } else {
        eprintln!(
            "\nTo resume: fabro resume {}",
            &run_id_string[..8.min(run_id_string.len())]
        );
    }

    Ok(())
}

pub(crate) fn timeline_entries_json(timeline: &RunTimeline) -> Vec<TimelineEntryJson> {
    timeline
        .entries
        .iter()
        .map(|entry| TimelineEntryJson {
            ordinal: entry.ordinal,
            node_name: entry.node_name.clone(),
            visit: entry.visit,
            run_commit_sha: entry.run_commit_sha.clone(),
        })
        .collect()
}

async fn reset_rewound_run_state(
    git_store: &Store,
    durable_store: &dyn fabro_store::Store,
    run_id: &fabro_types::RunId,
    run_dir: &std::path::Path,
) -> Result<()> {
    let run_record = RunRecord::load(run_dir)?;
    let checkpoint = MetadataStore::read_checkpoint(git_store.repo_dir(), &run_id.to_string())?
        .context("rewound metadata branch is missing checkpoint.json")?;
    checkpoint.save(&run_dir.join("checkpoint.json"))?;
    run_status::write_run_status(run_dir, RunStatus::Submitted, None);

    for name in [
        "conclusion.json",
        "pull_request.json",
        "detached_failure.json",
        "progress.jsonl",
        "retro.json",
        "final.patch",
    ] {
        let _ = std::fs::remove_file(run_dir.join(name));
    }

    durable_store
        .delete_run(run_id)
        .await
        .map_err(|err| anyhow::anyhow!("failed to reset durable store run: {err}"))?;
    let run_dir_string = run_dir.to_string_lossy().to_string();
    let run_store = durable_store
        .create_run(run_id, run_record.created_at, Some(&run_dir_string))
        .await
        .map_err(|err| anyhow::anyhow!("failed to recreate durable store run: {err}"))?;
    run_store
        .put_run(&run_record)
        .await
        .map_err(|err| anyhow::anyhow!("failed to restore run record after rewind: {err}"))?;
    if let Ok(start_record) = StartRecord::load(run_dir) {
        run_store
            .put_start(&start_record)
            .await
            .map_err(|err| anyhow::anyhow!("failed to restore start record after rewind: {err}"))?;
    }
    if let Ok(dot_source) = std::fs::read_to_string(run_dir.join("workflow.fabro")) {
        run_store
            .put_graph(&dot_source)
            .await
            .map_err(|err| anyhow::anyhow!("failed to restore graph after rewind: {err}"))?;
    }
    run_store
        .put_status(&fabro_types::RunStatusRecord::new(
            RunStatus::Submitted,
            None,
        ))
        .await
        .map_err(|err| anyhow::anyhow!("failed to restore run status after rewind: {err}"))?;
    run_store
        .put_checkpoint(&checkpoint)
        .await
        .map_err(|err| anyhow::anyhow!("failed to restore checkpoint after rewind: {err}"))?;
    Ok(())
}

pub(crate) fn print_timeline(timeline: &RunTimeline, styles: &Styles) {
    if timeline.entries.is_empty() {
        eprintln!("No checkpoints found.");
        return;
    }

    let use_color = styles.use_color;
    let title = vec![
        "@".cell().bold(use_color),
        "Node".cell().bold(use_color),
        "Details".cell().bold(use_color),
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

    let color_choice = if use_color {
        cli_table::ColorChoice::Auto
    } else {
        cli_table::ColorChoice::Never
    };
    let table = rows
        .table()
        .title(title)
        .color_choice(color_choice)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    #[allow(clippy::print_stderr)]
    if let Ok(display) = table.display() {
        eprintln!("{display}");
    }
}
