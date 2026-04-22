use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_util::text::strip_goal_decoration;
use fabro_workflow::run_status::RunStatus;

use super::short_run_id;
use crate::args::RunsListArgs;
use crate::command_context::CommandContext;
use crate::server_runs::{ServerSummaryLookup, filter_server_runs};
use crate::shared::{color_if, format_duration_ms, run_status_kind, tilde_path};

pub(crate) async fn list_command(
    args: &RunsListArgs,
    styles: &Styles,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    let label_filters = parse_label_filters(&args.filter.label);
    let filtered = filter_server_runs(
        lookup.runs(),
        args.filter.before.as_deref(),
        args.filter.workflow.as_deref(),
        &label_filters,
        !args.all,
    );

    if cli.output.format == OutputFormat::Json {
        let json_rows: Vec<_> = filtered
            .iter()
            .map(|run| {
                serde_json::json!({
                    "run_id": run.run_id(),
                    "workflow_name": run.workflow_name(),
                    "workflow_slug": run.workflow_slug(),
                    "status": run.status(),
                    "start_time": run.start_time(),
                    "labels": run.labels(),
                    "duration_ms": run.duration_ms(),
                    "total_usd_micros": run.total_usd_micros(),
                    "host_repo_path": run.host_repo_path(),
                    "goal": run.goal(),
                })
            })
            .collect();
        fabro_util::printout!(printer, "{}", serde_json::to_string_pretty(&json_rows)?);
        return Ok(());
    }

    if args.quiet {
        for run in &filtered {
            fabro_util::printout!(printer, "{}", run.run_id());
        }
        return Ok(());
    }

    if filtered.is_empty() {
        if args.all {
            fabro_util::printerr!(printer, "No runs found.");
        } else {
            fabro_util::printerr!(
                printer,
                "No running processes found. Use -a to show all runs (including archived)."
            );
        }
        return Ok(());
    }

    let mut display_runs = filtered;
    display_runs.reverse();

    let use_color = styles.use_color;
    let now = Utc::now();
    let title = vec![
        "RUN ID".cell().bold(use_color),
        "WORKFLOW".cell().bold(use_color),
        "STATUS".cell().bold(use_color),
        "DIRECTORY".cell().bold(use_color),
        "DURATION".cell().bold(use_color),
        "GOAL".cell().bold(use_color),
    ];

    let rows: Vec<Vec<CellStruct>> = display_runs
        .iter()
        .map(|run| {
            let duration_display = match run.duration_ms() {
                Some(ms) => format_duration_ms(ms),
                None => match run.start_time_dt() {
                    Some(start) => {
                        let elapsed = now.signed_duration_since(start);
                        format_duration_ms(elapsed.num_milliseconds().max(0).cast_unsigned())
                    }
                    None => "-".to_string(),
                },
            };
            let dir_display = run
                .host_repo_path()
                .map_or_else(|| "-".to_string(), |p| tilde_path(Path::new(p)));
            let run_id = run.run_id().to_string();

            vec![
                short_run_id(&run_id)
                    .cell()
                    .foreground_color(color_if(use_color, Color::Ansi256(8))),
                run.workflow_name().cell(),
                status_cell(run.status(), use_color),
                dir_display.cell(),
                duration_display.cell(),
                truncate_goal(&run.goal(), 50)
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
    fabro_util::printout!(printer, "{}", table.display()?);

    fabro_util::printerr!(printer, "\n{} run(s) listed.", display_runs.len());
    Ok(())
}

fn status_cell(status: RunStatus, use_color: bool) -> CellStruct {
    let text = run_status_kind(status);
    let color = match status {
        RunStatus::Succeeded { .. } => Some(Color::Green),
        RunStatus::Failed { .. } => Some(Color::Red),
        RunStatus::Running | RunStatus::Starting | RunStatus::Submitted => Some(Color::Cyan),
        RunStatus::Queued | RunStatus::Dead | RunStatus::Archived { .. } => Some(Color::Ansi256(8)),
        RunStatus::Blocked { .. } | RunStatus::Removing => Some(Color::Yellow),
        RunStatus::Paused { .. } => Some(Color::Magenta),
    };
    text.cell()
        .bold(use_color && color != Some(Color::Ansi256(8)))
        .foreground_color(color_if(use_color, color.unwrap_or(Color::Ansi256(8))))
}

fn parse_label_filters(label_args: &[String]) -> Vec<(String, String)> {
    label_args
        .iter()
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn truncate_goal(goal: &str, max_len: usize) -> String {
    truncate_str(strip_goal_decoration(goal), max_len)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_len - 3).collect();
    format!("{truncated}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_goal_strips_markdown_headings() {
        assert_eq!(truncate_goal("## Fix bug", 50), "Fix bug");
        assert_eq!(truncate_goal("# Title", 50), "Title");
        assert_eq!(truncate_goal("### Deep heading", 50), "Deep heading");
    }

    #[test]
    fn truncate_goal_strips_plan_prefix() {
        assert_eq!(truncate_goal("Plan: do stuff", 50), "do stuff");
    }

    #[test]
    fn truncate_goal_strips_heading_and_plan_prefix() {
        assert_eq!(truncate_goal("## Plan: migrate DB", 50), "migrate DB");
    }

    #[test]
    fn truncate_goal_plain_text_unchanged() {
        assert_eq!(truncate_goal("Fix the login bug", 50), "Fix the login bug");
    }

    #[test]
    fn truncate_goal_still_truncates_after_stripping() {
        assert_eq!(
            truncate_goal("## A long goal description", 10),
            "A long ..."
        );
    }
}
