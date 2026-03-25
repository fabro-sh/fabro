use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Args;
use cli_table::format::{Border, Justify, Separator};
use cli_table::{print_stdout, Cell, CellStruct, Color, Style, Table};
use fabro_util::terminal::Styles;
use tracing::{debug, info, warn};

use super::shared::{color_if, format_duration_ms, format_size, tilde_path};

#[derive(Args)]
pub struct RunFilterArgs {
    /// Only include runs started before this date (YYYY-MM-DD prefix match)
    #[arg(long)]
    pub before: Option<String>,

    /// Filter by workflow name (substring match)
    #[arg(long)]
    pub workflow: Option<String>,

    /// Filter by label (KEY=VALUE, repeatable, AND semantics)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,

    /// Include orphan directories (no run.json)
    #[arg(long)]
    pub orphans: bool,
}

#[derive(Args)]
pub struct RunsListArgs {
    #[command(flatten)]
    pub filter: RunFilterArgs,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Show all runs, not just running (like docker ps -a)
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Only display run IDs
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Args)]
pub struct RunsPruneArgs {
    #[command(flatten)]
    pub filter: RunFilterArgs,

    /// Only prune runs older than this duration (e.g. 24h, 7d). Default: 24h when no explicit filters are set.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub older_than: Option<chrono::Duration>,

    /// Actually delete (default is dry-run)
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct RunsRemoveArgs {
    /// Run IDs or workflow names to remove
    #[arg(required = true)]
    pub runs: Vec<String>,

    /// Force removal of active runs
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Args)]
pub struct DfArgs {
    /// Show per-run breakdown
    #[arg(short, long)]
    pub verbose: bool,
}

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

pub fn df_command(args: &DfArgs) -> Result<()> {
    let data_dir = fabro_workflows::run_lookup::default_data_dir();
    let runs_base = fabro_workflows::run_lookup::default_runs_base();
    let logs_base = fabro_workflows::run_lookup::default_logs_base();
    df_from(args, &data_dir, &runs_base, &logs_base)
}

pub fn prune_command(args: &RunsPruneArgs) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    prune_from(args, &base)
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

fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

fn df_from(args: &DfArgs, data_dir: &Path, runs_base: &Path, logs_base: &Path) -> Result<()> {
    let runs = fabro_workflows::run_lookup::scan_runs(runs_base)?;
    let mut active_count = 0u64;
    let mut total_run_size = 0u64;
    let mut reclaimable_run_size = 0u64;

    struct RunSizeInfo {
        run_id: String,
        workflow_name: String,
        status: fabro_workflows::run_status::RunStatus,
        start_time_dt: Option<DateTime<Utc>>,
        size: u64,
    }

    let mut run_details = Vec::new();
    for run in &runs {
        let size = dir_size(&run.path);
        total_run_size += size;
        if run.status.is_active() {
            active_count += 1;
        } else {
            reclaimable_run_size += size;
        }
        if args.verbose {
            run_details.push(RunSizeInfo {
                run_id: run.run_id.clone(),
                workflow_name: run.workflow_name.clone(),
                status: run.status,
                start_time_dt: run.start_time_dt,
                size,
            });
        }
    }

    let mut log_count = 0u64;
    let mut total_log_size = 0u64;
    if let Ok(entries) = std::fs::read_dir(logs_base) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "log") {
                if let Ok(meta) = path.metadata() {
                    log_count += 1;
                    total_log_size += meta.len();
                }
            }
        }
    }

    let mut db_count = 0u64;
    let mut total_db_size = 0u64;
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".db") || name.ends_with(".db-wal") || name.ends_with(".db-shm") {
                if let Ok(meta) = path.metadata() {
                    db_count += 1;
                    total_db_size += meta.len();
                }
            }
        }
    }

    let run_reclaim_pct = if total_run_size > 0 {
        (reclaimable_run_size as f64 / total_run_size as f64 * 100.0) as u64
    } else {
        0
    };
    let log_reclaim_pct = if total_log_size > 0 { 100 } else { 0 };

    let summary_title = vec![
        "TYPE".cell().bold(true),
        "COUNT".cell().bold(true).justify(Justify::Right),
        "ACTIVE".cell().bold(true).justify(Justify::Right),
        "SIZE".cell().bold(true).justify(Justify::Right),
        "RECLAIMABLE".cell().bold(true).justify(Justify::Right),
    ];
    let summary_rows: Vec<Vec<CellStruct>> = vec![
        vec![
            "Runs".cell(),
            runs.len().cell().justify(Justify::Right),
            active_count.cell().justify(Justify::Right),
            format_size(total_run_size).cell().justify(Justify::Right),
            format!("{} ({run_reclaim_pct}%)", format_size(reclaimable_run_size))
                .cell()
                .justify(Justify::Right),
        ],
        vec![
            "Logs".cell(),
            log_count.cell().justify(Justify::Right),
            "-".cell().justify(Justify::Right),
            format_size(total_log_size).cell().justify(Justify::Right),
            format!("{} ({log_reclaim_pct}%)", format_size(total_log_size))
                .cell()
                .justify(Justify::Right),
        ],
        vec![
            "Databases".cell(),
            db_count.cell().justify(Justify::Right),
            "-".cell().justify(Justify::Right),
            format_size(total_db_size).cell().justify(Justify::Right),
            format!("{} (0%)", format_size(0))
                .cell()
                .justify(Justify::Right),
        ],
    ];
    let summary_table = summary_rows
        .table()
        .title(summary_title)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    print_stdout(summary_table)?;

    println!();
    println!("Data directory: {}", data_dir.display());

    if !args.verbose {
        return Ok(());
    }

    println!();
    let verbose_title = vec![
        "RUN ID".cell().bold(true),
        "WORKFLOW".cell().bold(true),
        "STATUS".cell().bold(true),
        "AGE".cell().bold(true).justify(Justify::Right),
        "SIZE".cell().bold(true).justify(Justify::Right),
    ];

    let now = Utc::now();
    let verbose_rows: Vec<Vec<CellStruct>> = run_details
        .iter()
        .map(|detail| {
            let age = if let Some(dt) = detail.start_time_dt {
                let dur = now.signed_duration_since(dt);
                if dur.num_days() > 0 {
                    format!("{}d", dur.num_days())
                } else if dur.num_hours() > 0 {
                    format!("{}h", dur.num_hours())
                } else {
                    format!("{}m", dur.num_minutes().max(1))
                }
            } else {
                "-".to_string()
            };
            let size_display = if detail.status.is_active() {
                format_size(detail.size)
            } else {
                format!("{} *", format_size(detail.size))
            };
            vec![
                short_run_id(&detail.run_id).cell(),
                truncate_str(&detail.workflow_name, 16).cell(),
                detail.status.to_string().cell(),
                age.cell().justify(Justify::Right),
                size_display.cell().justify(Justify::Right),
            ]
        })
        .collect();
    let verbose_table = verbose_rows
        .table()
        .title(verbose_title)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    print_stdout(verbose_table)?;
    println!();
    println!("* = reclaimable");

    Ok(())
}

fn parse_duration(s: &str) -> Result<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration string");
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: u64 = num_str
        .parse()
        .with_context(|| format!("invalid duration: {s}"))?;
    match unit {
        "h" => Ok(chrono::Duration::hours(num as i64)),
        "d" => Ok(chrono::Duration::days(num as i64)),
        _ => bail!("invalid duration unit '{unit}' in '{s}' (expected 'h' or 'd')"),
    }
}

fn prune_from(args: &RunsPruneArgs, base: &Path) -> Result<()> {
    let runs = fabro_workflows::run_lookup::scan_runs(base)?;
    let label_filters = parse_label_filters(&args.filter.label);
    let mut filtered = fabro_workflows::run_lookup::filter_runs(
        &runs,
        args.filter.before.as_deref(),
        args.filter.workflow.as_deref(),
        &label_filters,
        args.filter.orphans,
        fabro_workflows::run_lookup::StatusFilter::All,
    );

    let has_explicit_filters =
        args.filter.before.is_some() || args.filter.workflow.is_some() || !label_filters.is_empty();
    let staleness_threshold = if let Some(duration) = args.older_than {
        Some(duration)
    } else if !has_explicit_filters {
        Some(chrono::Duration::hours(24))
    } else {
        None
    };

    if let Some(threshold) = staleness_threshold {
        let cutoff = Utc::now() - threshold;
        filtered.retain(|run| {
            if run.status.is_active() {
                return false;
            }
            run.end_time
                .or(run.start_time_dt)
                .is_some_and(|time| time < cutoff)
        });
    }

    if filtered.is_empty() {
        eprintln!("No matching runs to prune.");
        return Ok(());
    }

    let total_bytes: u64 = filtered.iter().map(|run| dir_size(&run.path)).sum();
    info!(count = filtered.len(), bytes = total_bytes, "pruning runs");

    if args.yes {
        for run in &filtered {
            info!(run_id = %run.run_id, path = %run.path.display(), "deleting run");
            std::fs::remove_dir_all(&run.path)?;
        }
        eprintln!(
            "{} run(s) deleted ({} freed).",
            filtered.len(),
            format_size(total_bytes)
        );
        return Ok(());
    }

    for run in &filtered {
        debug!(run_id = %run.run_id, "would delete run (dry-run)");
        println!("would delete: {} ({})", run.dir_name, run.workflow_name);
    }
    eprintln!(
        "\n{} run(s) would be deleted ({} freed). Pass --yes to confirm.",
        filtered.len(),
        format_size(total_bytes)
    );
    Ok(())
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
        if let Ok(record) = fabro_workflows::records::SandboxRecord::load(&sandbox_path) {
            if record.provider != "local" {
                match fabro_workflows::sandbox_reconnect::reconnect(&record).await {
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
