//! The checkpoint timeline as the CLI shows it: what `fabro timeline`
//! prints, and what `fabro fork --list` and `fabro rewind --list` show
//! before a target is chosen.

use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_api::types::{RunTimelineResponse, TimelineEntryResponse};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use serde::Serialize;

use crate::shared::color_if;

/// One timeline entry as `--json` prints it.
#[derive(Serialize)]
pub(crate) struct TimelineEntryJson {
    ordinal:        u64,
    node_name:      String,
    visit:          u32,
    stage:          Option<String>,
    execution:      u64,
    firing:         u64,
    attempt:        u32,
    run_commit_sha: Option<String>,
    files_changed:  Option<i64>,
}

/// The timeline as `--json` prints it.
#[derive(Serialize)]
pub(crate) struct TimelineJson {
    entries:     Vec<TimelineEntryJson>,
    forked_from: Option<ForkOriginJson>,
}

#[derive(Serialize)]
pub(crate) struct ForkOriginJson {
    source_run_id: String,
    execution:     u64,
    firing:        u64,
    rerun_last:    bool,
}

pub(crate) fn timeline_json(timeline: &RunTimelineResponse) -> TimelineJson {
    TimelineJson {
        entries:     timeline.entries.iter().map(entry_json).collect(),
        forked_from: timeline.forked_from.as_ref().map(|origin| ForkOriginJson {
            source_run_id: origin.source_run_id.clone(),
            execution:     origin.execution,
            firing:        origin.firing,
            rerun_last:    origin.rerun_last,
        }),
    }
}

fn entry_json(entry: &TimelineEntryResponse) -> TimelineEntryJson {
    TimelineEntryJson {
        ordinal:        entry.ordinal,
        node_name:      entry.node_name.clone(),
        visit:          entry.visit,
        stage:          entry.stage.clone(),
        execution:      entry.execution,
        firing:         entry.firing,
        attempt:        entry.attempt,
        run_commit_sha: entry.run_commit_sha.clone(),
        files_changed:  entry
            .diff_summary
            .as_ref()
            .map(|summary| summary.files_changed),
    }
}

pub(crate) fn short_id(run_id: &str) -> &str {
    &run_id[..8.min(run_id.len())]
}

/// Print the timeline as a table on stderr, with where the run was forked
/// from when it is a fork.
pub(crate) fn print_timeline(timeline: &RunTimelineResponse, styles: &Styles, printer: Printer) {
    if let Some(origin) = &timeline.forked_from {
        fabro_util::printerr!(
            printer,
            "Forked from {} at execution {} firing {}{}",
            short_id(&origin.source_run_id),
            origin.execution,
            origin.firing,
            if origin.rerun_last {
                " (that stage runs again)"
            } else {
                ""
            }
        );
    }
    if timeline.entries.is_empty() {
        fabro_util::printerr!(printer, "No checkpoints found.");
        return;
    }

    let use_color = styles.use_color;
    let title = vec![
        "@".cell().bold(use_color),
        "Node".cell().bold(use_color),
        "Commit".cell().bold(use_color),
        "Details".cell().bold(use_color),
    ];

    let rows: Vec<Vec<CellStruct>> = timeline
        .entries
        .iter()
        .map(|entry| {
            let mut details = Vec::new();
            if entry.visit > 1 {
                details.push(format!("visit {}, loop", entry.visit));
            }
            if entry.attempt > 1 {
                details.push(format!("attempt {}", entry.attempt));
            }
            if let Some(summary) = &entry.diff_summary {
                details.push(format!(
                    "{} files, +{} -{}",
                    summary.files_changed, summary.additions, summary.deletions
                ));
            }
            let commit = entry.run_commit_sha.as_deref().map_or_else(
                || "no run commit".to_string(),
                |sha| short_id(sha).to_string(),
            );
            let detail_str = if details.is_empty() {
                String::new()
            } else {
                format!("({})", details.join(", "))
            };

            vec![
                format!("@{}", entry.ordinal)
                    .cell()
                    .foreground_color(color_if(use_color, Color::Cyan)),
                entry.node_name.clone().cell(),
                commit.cell(),
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
    if let Ok(display) = table.display() {
        for line in display.to_string().lines() {
            fabro_util::printerr!(printer, "{}", line.trim_end());
        }
    }
}
