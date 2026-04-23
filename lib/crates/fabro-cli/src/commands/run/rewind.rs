use anyhow::{Context, Result};
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_checkpoint::git::Store;
use fabro_types::run_event::{CheckpointCompletedProps, RunRewoundProps, RunSubmittedProps};
use fabro_types::{EventBody, RunEvent};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_workflow::git::MetadataStore;
use fabro_workflow::operations::{
    RewindInput, RewindTarget, RunTimeline, TimelineEntry, build_timeline_or_rebuild, rewind,
};
use git2::Repository;
use serde::Serialize;

use crate::args::RewindArgs;
use crate::command_context::CommandContext;
use crate::commands::store::rebuild::rebuild_run_store;
use crate::server_client::Client;
use crate::shared::repo::ensure_matching_repo_origin;
use crate::shared::{color_if, print_json_pretty};

#[derive(Serialize)]
pub(crate) struct TimelineEntryJson {
    ordinal:        usize,
    node_name:      String,
    visit:          usize,
    run_commit_sha: Option<String>,
}

pub(crate) async fn run(
    args: &RewindArgs,
    styles: &Styles,
    base_ctx: &CommandContext,
) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run_id).await?.run_id;
    let state = client.get_run_state(&run_id).await?;
    let current_status = state
        .status
        .context("run has no recorded status — cannot rewind")?;
    let run_spec = state.spec.context("Failed to load run spec from store")?;
    ensure_matching_repo_origin(run_spec.repo_origin_url.as_deref(), "rewind")?;
    let store = Store::new(repo);
    let events = client.list_run_events(&run_id, None, None).await?;
    let run_store = rebuild_run_store(&run_id, &events).await?;

    let timeline = build_timeline_or_rebuild(&store, Some(&run_store), &run_id).await?;

    if args.list || args.target.is_none() {
        if ctx.json_output() {
            print_json_pretty(&timeline_entries_json(&timeline))?;
            return Ok(());
        }
        print_timeline(&timeline, styles, printer);
        return Ok(());
    }

    let target_arg = args
        .target
        .as_deref()
        .expect("rewind target should be present unless listing");
    let target = target_arg.parse::<RewindTarget>()?;

    rewind(&store, &RewindInput {
        run_id,
        target: target.clone(),
        push: !args.no_push,
        current_status,
    })?;
    let entry = timeline.resolve(&target)?;
    reset_rewound_run_state(client.as_ref(), &store, &run_id, entry).await?;

    let run_id_string = run_id.to_string();

    if ctx.json_output() {
        print_json_pretty(&serde_json::json!({
            "run_id": run_id_string,
            "target": target_arg,
        }))?;
    } else {
        fabro_util::printerr!(
            printer,
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
            ordinal:        entry.ordinal,
            node_name:      entry.node_name.clone(),
            visit:          entry.visit,
            run_commit_sha: entry.run_commit_sha.clone(),
        })
        .collect()
}

async fn reset_rewound_run_state(
    client: &Client,
    git_store: &Store,
    run_id: &fabro_types::RunId,
    entry: &TimelineEntry,
) -> Result<()> {
    let state = client.get_run_state(run_id).await.map_err(|err| {
        anyhow::anyhow!("failed to load durable store state before rewind: {err}")
    })?;

    let definition_blob = state.spec.as_ref().and_then(|run| run.definition_blob);
    if state.spec.is_none() {
        anyhow::bail!("failed to restore run spec after rewind: missing run metadata");
    }
    let checkpoint = MetadataStore::read_run_projection(git_store.repo_dir(), &run_id.to_string())?
        .and_then(|projection| projection.checkpoint)
        .context("rewound metadata branch is missing run.json checkpoint state")?;
    let previous_status = state.status.map(|status| status.to_string());

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
                EventBody::RunSubmitted(RunSubmittedProps { definition_blob }),
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

fn run_event(run_id: fabro_types::RunId, node_id: Option<String>, body: EventBody) -> RunEvent {
    RunEvent {
        id: ulid::Ulid::new().to_string(),
        ts: chrono::Utc::now(),
        run_id,
        node_id,
        node_label: None,
        stage_id: None,
        parallel_group_id: None,
        parallel_branch_id: None,
        session_id: None,
        parent_session_id: None,
        tool_call_id: None,
        actor: None,
        body,
    }
}

pub(crate) fn print_timeline(timeline: &RunTimeline, styles: &Styles, printer: Printer) {
    if timeline.entries.is_empty() {
        fabro_util::printerr!(printer, "No checkpoints found.");
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
    #[allow(
        clippy::print_stderr,
        reason = "The rewind preview table is operator feedback, not command output."
    )]
    if let Ok(display) = table.display() {
        eprintln!("{display}");
    }
}
