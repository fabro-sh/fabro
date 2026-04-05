use anyhow::Context;
use anyhow::Result;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_checkpoint::git::Store;
use fabro_util::terminal::Styles;
use fabro_types::run_event::{
    CheckpointCompletedProps, RunRewoundProps, RunStatusTransitionProps,
};
use fabro_workflow::git::MetadataStore;
use fabro_workflow::operations::{
    RewindInput, RewindTarget, RunTimeline, TimelineEntry, build_timeline_or_rebuild, rewind,
};
use fabro_types::{EventBody, RunEvent};
use fabro_workflow::run_lookup::{resolve_run_from_summaries, runs_base};
use git2::Repository;
use serde::Serialize;

use crate::args::{GlobalArgs, RewindArgs};
use crate::commands::store::rebuild::rebuild_run_store;
use crate::server_client;
use crate::shared::{color_if, print_json_pretty};
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
    let client = server_client::connect_server(&cli_settings.storage_dir()).await?;
    let base = runs_base(&cli_settings.storage_dir());
    let summaries = client.list_store_runs().await?;
    let run = resolve_run_from_summaries(&summaries, &base, &args.run_id)?;
    let run_id = run.run_id();
    let store = Store::new(repo);
    let events = client.list_run_events(&run_id, None, None).await?;
    let run_store = rebuild_run_store(&run_id, &events).await?;

    let timeline = build_timeline_or_rebuild(&store, Some(&run_store), &run_id).await?;

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
            target: target.clone(),
            push: !args.no_push,
        },
    )?;
    let entry = timeline.resolve(&target)?;
    reset_rewound_run_state(&client, &store, &run_id, &run.path, entry).await?;

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
    client: &crate::server_client::ServerStoreClient,
    git_store: &Store,
    run_id: &fabro_types::RunId,
    run_dir: &std::path::Path,
    entry: &TimelineEntry,
) -> Result<()> {
    let state = client
        .get_run_state(run_id)
        .await
        .map_err(|err| anyhow::anyhow!("failed to load durable store state before rewind: {err}"))?;

    let _run_record = state
        .run
        .context("failed to restore run record after rewind: missing run metadata")?;
    let checkpoint = MetadataStore::read_checkpoint(git_store.repo_dir(), &run_id.to_string())?
        .context("rewound metadata branch is missing checkpoint.json")?;
    let previous_status = state.status.map(|status| status.status.to_string());

    let _ = std::fs::remove_file(run_dir.join("detached_failure.json"));

    client
        .append_run_event(
            run_id,
            &run_event(
                *run_id,
                None,
                EventBody::RunRewound(RunRewoundProps {
                    target_checkpoint_ordinal: entry.ordinal,
                    target_node_id: entry.node_name.clone(),
                    target_visit: entry.visit,
                    previous_status,
                    run_commit_sha: entry.run_commit_sha.clone(),
                }),
            ),
        )
        .await
        .map_err(|err| anyhow::anyhow!("failed to append run rewound event: {err}"))?;
    client
        .append_run_event(run_id, &restored_checkpoint_event(*run_id, &checkpoint))
        .await
        .map_err(|err| anyhow::anyhow!("failed to append restored checkpoint event: {err}"))?;
    client
        .append_run_event(
            run_id,
            &run_event(
                *run_id,
                None,
                EventBody::RunSubmitted(RunStatusTransitionProps { reason: None }),
            ),
        )
        .await
        .map_err(|err| anyhow::anyhow!("failed to append restored run status event: {err}"))?;
    Ok(())
}

fn restored_checkpoint_event(
    run_id: fabro_types::RunId,
    checkpoint: &fabro_types::Checkpoint,
) -> RunEvent {
    let current_status = checkpoint
        .node_outcomes
        .get(&checkpoint.current_node)
        .map_or_else(
            || "success".to_string(),
            |outcome| outcome.status.to_string(),
        );
    run_event(
        run_id,
        Some(checkpoint.current_node.clone()),
        EventBody::CheckpointCompleted(CheckpointCompletedProps {
            status: current_status,
            current_node: checkpoint.current_node.clone(),
            completed_nodes: checkpoint.completed_nodes.clone(),
            node_retries: checkpoint.node_retries.clone().into_iter().collect(),
            context_values: checkpoint.context_values.clone().into_iter().collect(),
            node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
            next_node_id: checkpoint.next_node_id.clone(),
            git_commit_sha: checkpoint.git_commit_sha.clone(),
            loop_failure_signatures: checkpoint
                .loop_failure_signatures
                .iter()
                .map(|(sig, count)| (sig.to_string(), *count))
                .collect(),
            restart_failure_signatures: checkpoint
                .restart_failure_signatures
                .iter()
                .map(|(sig, count)| (sig.to_string(), *count))
                .collect(),
            node_visits: checkpoint.node_visits.clone().into_iter().collect(),
            diff: None,
        }),
    )
}

fn run_event(
    run_id: fabro_types::RunId,
    node_id: Option<String>,
    body: EventBody,
) -> RunEvent {
    RunEvent {
        id: ulid::Ulid::new().to_string(),
        ts: chrono::Utc::now(),
        run_id,
        node_id,
        node_label: None,
        session_id: None,
        parent_session_id: None,
        body,
    }
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
