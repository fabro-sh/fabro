use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use cli_table::format::{Border, Separator};
use cli_table::{print_stdout, Cell, CellStruct, Color, Style, Table};
use fabro_util::terminal::Styles;
use tracing::warn;

use crate::args::{RunsListArgs, RunsRemoveArgs};
use crate::shared::{color_if, format_duration_ms, tilde_path};

pub fn list_command(args: &RunsListArgs, styles: &Styles) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    let runs = fabro_workflows::run_lookup::scan_runs(&base)?;
    let label_filters = parse_label_filters(&args.filter.label);
    let filtered = fabro_workflows::run_lookup::filter_runs(
        &runs,
        args.filter.before.as_deref(),
        args.filter.workflow.as_deref(),
        &label_filters,
        args.filter.orphans,
        if args.all {
            fabro_workflows::run_lookup::StatusFilter::All
        } else {
            fabro_workflows::run_lookup::StatusFilter::RunningOnly
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
                        format_duration_ms(elapsed.num_milliseconds().max(0) as u64)
                    }
                    None => "-".to_string(),
                },
            };
            let dir_display = run
                .host_repo_path
                .as_deref()
                .map(|p| tilde_path(Path::new(p)))
                .unwrap_or_else(|| "-".to_string());

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

pub async fn remove_command(args: &RunsRemoveArgs) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    remove_from(args, &base).await
}

fn status_cell(status: fabro_workflows::run_status::RunStatus, use_color: bool) -> CellStruct {
    let text = status.to_string();
    let color = match status {
        fabro_workflows::run_status::RunStatus::Succeeded => Some(Color::Green),
        fabro_workflows::run_status::RunStatus::Failed => Some(Color::Red),
        fabro_workflows::run_status::RunStatus::Running
        | fabro_workflows::run_status::RunStatus::Starting
        | fabro_workflows::run_status::RunStatus::Submitted => Some(Color::Cyan),
        fabro_workflows::run_status::RunStatus::Removing => Some(Color::Yellow),
        fabro_workflows::run_status::RunStatus::Paused => Some(Color::Magenta),
        fabro_workflows::run_status::RunStatus::Dead => Some(Color::Ansi256(8)),
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

fn short_run_id(id: &str) -> &str {
    if id.len() > 12 {
        &id[..12]
    } else {
        id
    }
}

fn truncate_goal(goal: &str, max_len: usize) -> String {
    truncate_str(fabro_util::text::strip_goal_decoration(goal), max_len)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_len - 3).collect();
    format!("{truncated}...")
}

async fn remove_from(args: &RunsRemoveArgs, base: &Path) -> Result<()> {
    let mut had_errors = false;

    for identifier in &args.runs {
        let run = match fabro_workflows::run_lookup::resolve_run(base, identifier) {
            Ok(run) => run,
            Err(err) => {
                eprintln!("error: {identifier}: {err}");
                had_errors = true;
                continue;
            }
        };

        if run.status.is_active() && !args.force {
            eprintln!(
                "cannot remove active run {} (status: {}, use -f to force)",
                short_run_id(&run.run_id),
                run.status
            );
            had_errors = true;
            continue;
        }

        fabro_workflows::run_status::write_run_status(
            &run.path,
            fabro_workflows::run_status::RunStatus::Removing,
            None,
        );

        let sandbox_path = run.path.join("sandbox.json");
        if let Ok(record) = fabro_sandbox::SandboxRecord::load(&sandbox_path) {
            if record.provider != "local" {
                match fabro_sandbox::reconnect::reconnect(&record).await {
                    Ok(sandbox) => {
                        if let Err(err) = sandbox.cleanup().await {
                            warn!(run_id = %run.run_id, error = %err, "sandbox cleanup failed");
                        }
                    }
                    Err(err) => {
                        warn!(run_id = %run.run_id, error = %err, "sandbox reconnect failed");
                    }
                }
            }
        }

        std::fs::remove_dir_all(&run.path)
            .with_context(|| format!("failed to delete {}", run.path.display()))?;
        eprintln!("{}", short_run_id(&run.run_id));
    }

    if had_errors {
        bail!("some runs could not be removed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::system::parse_duration;
    use crate::shared::format_size;

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("24h").unwrap(), chrono::Duration::hours(24));
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration("7d").unwrap(), chrono::Duration::days(7));
    }

    #[test]
    fn parse_duration_rejects_invalid_unit() {
        let err = parse_duration("5m").unwrap_err();
        assert!(err.to_string().contains("invalid duration unit"));
    }

    #[test]
    fn format_size_humanizes_thresholds() {
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }

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
