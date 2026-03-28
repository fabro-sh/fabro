use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use cli_table::format::{Border, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table, print_stdout};
use fabro_config::FabroSettingsExt;
use fabro_util::terminal::Styles;

use fabro_util::text::strip_goal_decoration;
use fabro_workflows::run_lookup::{StatusFilter, filter_runs, runs_base, scan_runs};
use fabro_workflows::run_status::RunStatus;

use crate::args::RunsListArgs;
use crate::cli_config::load_cli_settings;
use crate::shared::{color_if, format_duration_ms, tilde_path};

use super::short_run_id;

pub(crate) fn list_command(args: &RunsListArgs, styles: &Styles) -> Result<()> {
    let cli_settings = load_cli_settings(None)?;
    let base = runs_base(&cli_settings.storage_dir());
    let runs = scan_runs(&base)?;
    let label_filters = parse_label_filters(&args.filter.label);
    let filtered = filter_runs(
        &runs,
        args.filter.before.as_deref(),
        args.filter.workflow.as_deref(),
        &label_filters,
        args.filter.orphans,
        if args.all {
            StatusFilter::All
        } else {
            StatusFilter::RunningOnly
        },
    );

    if args.quiet {
        for run in &filtered {
            println!("{}", run.run_id);
        }
        return Ok(());
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
        return Ok(());
    }

    if filtered.is_empty() {
        if args.all {
            eprintln!("No runs found.");
        } else {
            eprintln!("No running processes found. Use -a to show all runs.");
        }
        return Ok(());
    }

    let mut display_runs = filtered;
    display_runs.reverse();

    let use_color = styles.use_color;
    let now = Utc::now();
    let title = vec![
        "RUN ID".cell().bold(true),
        "WORKFLOW".cell().bold(true),
        "STATUS".cell().bold(true),
        "DIRECTORY".cell().bold(true),
        "DURATION".cell().bold(true),
        "GOAL".cell().bold(true),
    ];

    let rows: Vec<Vec<CellStruct>> = display_runs
        .iter()
        .map(|run| {
            let duration_display = match run.duration_ms {
                Some(ms) => format_duration_ms(ms),
                None => match run.start_time_dt {
                    Some(start) => {
                        let elapsed = now.signed_duration_since(start);
                        format_duration_ms(
                            u64::try_from(elapsed.num_milliseconds().max(0)).unwrap(),
                        )
                    }
                    None => "-".to_string(),
                },
            };
            let dir_display = run
                .host_repo_path
                .as_deref()
                .map_or_else(|| "-".to_string(), |p| tilde_path(Path::new(p)));

            vec![
                short_run_id(&run.run_id)
                    .cell()
                    .foreground_color(color_if(use_color, Color::Ansi256(8))),
                run.workflow_name.clone().cell(),
                status_cell(run.status, use_color),
                dir_display.cell(),
                duration_display.cell(),
                truncate_goal(&run.goal, 50)
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
    print_stdout(table)?;

    eprintln!("\n{} run(s) listed.", display_runs.len());
    Ok(())
}

fn status_cell(status: RunStatus, use_color: bool) -> CellStruct {
    let text = status.to_string();
    let color = match status {
        RunStatus::Succeeded => Some(Color::Green),
        RunStatus::Failed => Some(Color::Red),
        RunStatus::Running | RunStatus::Starting | RunStatus::Submitted => Some(Color::Cyan),
        RunStatus::Removing => Some(Color::Yellow),
        RunStatus::Paused => Some(Color::Magenta),
        RunStatus::Dead => Some(Color::Ansi256(8)),
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
