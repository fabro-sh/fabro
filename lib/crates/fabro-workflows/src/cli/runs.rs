use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Args;
use cli_table::format::{Border, Justify, Separator};
use cli_table::{print_stdout, Cell, CellStruct, Color, Style, Table};
use fabro_util::terminal::Styles;
use serde::Serialize;
use tracing::{debug, info, warn};

pub use crate::run_status::{RunStatus, RunStatusRecord, StatusReason};

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

    /// Include orphan directories (no manifest.json)
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

#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub run_id: String,
    pub dir_name: String,
    pub workflow_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug: Option<String>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<StatusReason>,
    pub start_time: String,
    pub labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_repo_path: Option<String>,
    pub goal: String,
    #[serde(skip)]
    pub start_time_dt: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub path: PathBuf,
    #[serde(skip)]
    pub is_orphan: bool,
}

/// Scan a logs base directory and return info about each run.
pub fn scan_runs(base: &Path) -> Result<Vec<RunInfo>> {
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    let mut runs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = entry.file_name().to_string_lossy().to_string();

        debug!(dir = %dir_name, "scanning run directory");

        let manifest_path = path.join("manifest.json");
        if let Ok(manifest) = crate::manifest::Manifest::load(&manifest_path) {
            debug!(dir = %dir_name, "reading manifest");

            let run_id = manifest.run_id;
            let workflow_name = manifest.workflow_name;
            let workflow_slug = manifest.workflow_slug;
            let host_repo_path = manifest.host_repo_path;
            let goal = manifest.goal;
            let start_time_dt = manifest.start_time;
            let start_time = start_time_dt.to_rfc3339();
            let labels = manifest.labels;

            let si = read_status(&path);

            runs.push(RunInfo {
                run_id,
                dir_name,
                workflow_name,
                workflow_slug,
                status: si.status,
                status_reason: si.reason,
                start_time,
                labels,
                duration_ms: si.duration_ms,
                total_cost: si.total_cost,
                host_repo_path,
                start_time_dt: Some(start_time_dt),
                end_time: si.end_time,
                path,
                goal,
                is_orphan: false,
            });
        } else {
            // No manifest.json — check for status.json (starting run) vs true orphan
            let mtime_dt = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| -> DateTime<Utc> { t.into() });
            let mtime = mtime_dt.map(|dt| dt.to_rfc3339()).unwrap_or_default();

            let run_id = std::fs::read_to_string(path.join("id.txt"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| dir_name.clone());

            let si = read_status(&path);
            let is_orphan = matches!(si.status, RunStatus::Dead);
            runs.push(RunInfo {
                run_id,
                dir_name,
                workflow_name: if is_orphan {
                    "[no manifest]"
                } else {
                    "[starting]"
                }
                .to_string(),
                workflow_slug: None,
                status: si.status,
                status_reason: si.reason,
                start_time: mtime,
                labels: HashMap::new(),
                duration_ms: si.duration_ms,
                total_cost: si.total_cost,
                host_repo_path: None,
                start_time_dt: mtime_dt,
                end_time: si.end_time,
                path,
                goal: String::new(),
                is_orphan,
            });
        }
    }

    // Sort by start_time descending (newest first)
    runs.sort_by(|a, b| b.start_time.cmp(&a.start_time));
    Ok(runs)
}

struct StatusInfo {
    status: RunStatus,
    reason: Option<StatusReason>,
    end_time: Option<DateTime<Utc>>,
    duration_ms: Option<u64>,
    total_cost: Option<f64>,
}

impl StatusInfo {
    fn simple(status: RunStatus) -> Self {
        Self {
            status,
            reason: None,
            end_time: None,
            duration_ms: None,
            total_cost: None,
        }
    }
}

/// Write the run status to `status.json` (best-effort).
pub fn write_run_status(run_dir: &Path, status: RunStatus, reason: Option<StatusReason>) {
    let record = RunStatusRecord::new(status, reason);
    if let Err(e) = record.save(&run_dir.join("status.json")) {
        warn!("failed to write status.json for {}: {e}", run_dir.display());
    }
}

fn read_status(run_dir: &Path) -> StatusInfo {
    // 1. status.json is authoritative
    if let Ok(record) = RunStatusRecord::load(&run_dir.join("status.json")) {
        // For terminal statuses, pull duration/cost from conclusion.json if available
        if record.status.is_terminal() {
            if let Ok(conclusion) =
                crate::conclusion::Conclusion::load(&run_dir.join("conclusion.json"))
            {
                return StatusInfo {
                    status: record.status,
                    reason: record.reason,
                    end_time: Some(conclusion.timestamp),
                    duration_ms: Some(conclusion.duration_ms),
                    total_cost: conclusion.total_cost,
                };
            }
        }
        return StatusInfo {
            status: record.status,
            reason: record.reason,
            end_time: None,
            duration_ms: None,
            total_cost: None,
        };
    }
    // No status.json → Dead (orphan)
    StatusInfo::simple(RunStatus::Dead)
}

/// Which run statuses to include in filtered results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    /// Only include runs that are currently running.
    RunningOnly,
    /// Include runs of any status.
    All,
}

/// Filter runs by criteria. Orphans are excluded unless `include_orphans` is true.
pub fn filter_runs(
    runs: &[RunInfo],
    before: Option<&str>,
    workflow: Option<&str>,
    labels: &[(String, String)],
    include_orphans: bool,
    status_filter: StatusFilter,
) -> Vec<RunInfo> {
    runs.iter()
        .filter(|r| {
            if status_filter == StatusFilter::RunningOnly && !r.status.is_active() {
                return false;
            }
            if r.is_orphan && !include_orphans {
                return false;
            }
            if let Some(before) = before {
                if !r.start_time.is_empty() && r.start_time.as_str() >= before {
                    return false;
                }
            }
            if let Some(pat) = workflow {
                if !r.workflow_name.contains(pat) {
                    return false;
                }
            }
            for (k, v) in labels {
                match r.labels.get(k) {
                    Some(val) if val == v => {}
                    _ => return false,
                }
            }
            true
        })
        .cloned()
        .collect()
}

fn parse_label_filters(label_args: &[String]) -> Vec<(String, String)> {
    label_args
        .iter()
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".fabro")
}

pub(crate) fn default_runs_base() -> PathBuf {
    default_data_dir().join("runs")
}

pub(crate) fn default_logs_base() -> PathBuf {
    default_data_dir().join("logs")
}

/// Find a run directory by prefix match against run IDs.
pub fn find_run_by_prefix(base: &Path, prefix: &str) -> Result<PathBuf> {
    let runs = scan_runs(base).context("Failed to scan runs")?;
    let matches: Vec<_> = runs
        .iter()
        .filter(|r| r.run_id.starts_with(prefix))
        .collect();

    match matches.len() {
        0 => {
            warn!(run_id = %prefix, "No matching run found");
            bail!("No run found matching prefix '{prefix}'")
        }
        1 => {
            let run = &matches[0];
            debug!(run_id = %prefix, matched = %run.run_id, "Resolved run by prefix");
            Ok(run.path.clone())
        }
        n => {
            let ids: Vec<&str> = matches.iter().map(|r| r.run_id.as_str()).collect();
            bail!(
                "Ambiguous prefix '{prefix}': {n} runs match: {}",
                ids.join(", ")
            )
        }
    }
}

/// Resolve a user-supplied identifier to a `RunInfo`.
///
/// Resolution order:
/// 1. Run ID prefix match (like `find_run_by_prefix`)
/// 2. Workflow name substring match, returning the most recent run
///
/// Errors if no match is found, or if a run ID prefix is ambiguous.
pub fn resolve_run(base: &Path, identifier: &str) -> Result<RunInfo> {
    let runs = scan_runs(base).context("Failed to scan runs")?;

    // Step 1: try run ID prefix match
    let id_matches: Vec<_> = runs
        .iter()
        .filter(|r| r.run_id.starts_with(identifier))
        .collect();

    match id_matches.len() {
        1 => {
            debug!(identifier, matched = %id_matches[0].run_id, "Resolved run by ID prefix");
            return Ok(id_matches[0].clone());
        }
        n if n > 1 => {
            let ids: Vec<&str> = id_matches.iter().map(|r| r.run_id.as_str()).collect();
            bail!(
                "Ambiguous prefix '{identifier}': {n} runs match: {}",
                ids.join(", ")
            )
        }
        _ => {}
    }

    // Step 2: try workflow slug (exact, case-insensitive) then workflow name match.
    // Returns the most recent run (runs are sorted newest-first).
    let id_lower = identifier.to_lowercase();
    let id_collapsed = collapse_separators(&id_lower);
    let wf_match = runs.iter().filter(|r| !r.is_orphan).find(|r| {
        // Exact slug match (case-insensitive)
        if let Some(slug) = &r.workflow_slug {
            if slug.to_lowercase() == id_lower {
                return true;
            }
        }
        // Fuzzy workflow name match (substring, separator-collapsed)
        let name_lower = r.workflow_name.to_lowercase();
        name_lower.contains(&id_lower) || collapse_separators(&name_lower).contains(&id_collapsed)
    });

    match wf_match {
        Some(run) => {
            debug!(identifier, matched = %run.run_id, workflow = %run.workflow_name, "Resolved run by workflow name");
            Ok(run.clone())
        }
        None => {
            warn!(identifier, "No matching run found");
            bail!("No run found matching '{identifier}' (tried run ID prefix and workflow name)")
        }
    }
}

/// Strip hyphens and underscores so "legacy-tool" collapses to "legacytool",
/// matching PascalCase "LegacyTool" after lowercasing.
fn collapse_separators(s: &str) -> String {
    s.chars().filter(|c| *c != '-' && *c != '_').collect()
}

/// Truncate a run ID to a short display form (12 characters).
fn short_run_id(id: &str) -> &str {
    if id.len() > 12 {
        &id[..12]
    } else {
        id
    }
}

fn truncate_goal(goal: &str, max_len: usize) -> String {
    let line = goal.lines().next().unwrap_or("");
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max_len {
        return line.to_string();
    }
    let truncated: String = chars[..max_len - 3].iter().collect();
    format!("{truncated}...")
}

use super::color_if;

fn status_cell(status: &RunStatus, use_color: bool) -> CellStruct {
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

pub fn list_command(args: &RunsListArgs, styles: &Styles) -> Result<()> {
    let base = default_runs_base();
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

    // Reverse to oldest-first for display (scan_runs returns newest-first)
    let mut display_runs = filtered;
    display_runs.reverse();

    let use_color = styles.use_color;
    let title = vec![
        "RUN ID".cell().bold(true),
        "WORKFLOW".cell().bold(true),
        "STATUS".cell().bold(true),
        "DIRECTORY".cell().bold(true),
        "DURATION".cell().bold(true),
        "GOAL".cell().bold(true),
    ];

    let rows: Vec<Vec<CellStruct>> =
        display_runs
            .iter()
            .map(|run| {
                let run_id_display = short_run_id(&run.run_id);
                let duration_display = match run.duration_ms {
                    Some(ms) => super::progress::format_duration_ms(ms),
                    None => match run.start_time_dt {
                        Some(start) => {
                            let elapsed = Utc::now().signed_duration_since(start);
                            super::progress::format_duration_ms(
                                elapsed.num_milliseconds().max(0) as u64
                            )
                        }
                        None => "-".to_string(),
                    },
                };
                let dir_display = run
                    .host_repo_path
                    .as_deref()
                    .map(|p| super::tilde_path(Path::new(p)))
                    .unwrap_or_else(|| "-".to_string());
                vec![
                    run_id_display
                        .cell()
                        .foreground_color(color_if(use_color, Color::Ansi256(8))),
                    run.workflow_name.clone().cell(),
                    status_cell(&run.status, use_color),
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

#[derive(Args)]
pub struct DfArgs {
    /// Show per-run breakdown
    #[arg(short, long)]
    pub verbose: bool,
}

fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

pub(crate) fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub fn df_command(args: &DfArgs) -> Result<()> {
    let data_dir = default_data_dir();
    let runs_base = default_runs_base();
    let logs_base = default_logs_base();
    df_from(args, &data_dir, &runs_base, &logs_base)
}

pub fn df_from(args: &DfArgs, data_dir: &Path, runs_base: &Path, logs_base: &Path) -> Result<()> {
    // --- Runs ---
    let runs = scan_runs(runs_base)?;
    let mut active_count = 0u64;
    let mut total_run_size = 0u64;
    let mut reclaimable_run_size = 0u64;

    struct RunSizeInfo {
        run_id: String,
        workflow_name: String,
        status: RunStatus,
        start_time_dt: Option<DateTime<Utc>>,
        size: u64,
    }

    let mut run_details: Vec<RunSizeInfo> = Vec::new();

    for run in &runs {
        let size = dir_size(&run.path);
        total_run_size += size;
        let is_active = run.status.is_active();
        if is_active {
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

    // --- Logs ---
    let mut log_count = 0u64;
    let mut total_log_size = 0u64;
    if let Ok(entries) = std::fs::read_dir(logs_base) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "log" {
                        if let Ok(meta) = path.metadata() {
                            log_count += 1;
                            total_log_size += meta.len();
                        }
                    }
                }
            }
        }
    }

    // --- Databases ---
    let mut db_count = 0u64;
    let mut total_db_size = 0u64;
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".db") || name.ends_with(".db-wal") || name.ends_with(".db-shm") {
                    if let Ok(meta) = path.metadata() {
                        db_count += 1;
                        total_db_size += meta.len();
                    }
                }
            }
        }
    }

    // --- Summary table ---
    let run_reclaim_pct = if total_run_size > 0 {
        (reclaimable_run_size as f64 / total_run_size as f64 * 100.0) as u64
    } else {
        0
    };
    let log_reclaim_pct = if total_log_size > 0 { 100 } else { 0 };

    let df_title = vec![
        "TYPE".cell().bold(true),
        "COUNT".cell().bold(true).justify(Justify::Right),
        "ACTIVE".cell().bold(true).justify(Justify::Right),
        "SIZE".cell().bold(true).justify(Justify::Right),
        "RECLAIMABLE".cell().bold(true).justify(Justify::Right),
    ];
    let df_rows: Vec<Vec<CellStruct>> = vec![
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
    let df_table = df_rows
        .table()
        .title(df_title)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    print_stdout(df_table)?;

    println!();
    println!("Data directory: {}", data_dir.display());

    // --- Verbose per-run breakdown ---
    if args.verbose {
        println!();
        let verbose_title = vec![
            "RUN ID".cell().bold(true),
            "WORKFLOW".cell().bold(true),
            "STATUS".cell().bold(true),
            "AGE".cell().bold(true).justify(Justify::Right),
            "SIZE".cell().bold(true).justify(Justify::Right),
        ];

        let now = chrono::Utc::now();
        let verbose_rows: Vec<Vec<CellStruct>> = run_details
            .iter()
            .map(|detail| {
                let run_id_display = short_run_id(&detail.run_id);
                let workflow_display = truncate_goal(&detail.workflow_name, 16);
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
                let size_display = if !detail.status.is_active() {
                    format!("{} *", format_size(detail.size))
                } else {
                    format_size(detail.size)
                };
                vec![
                    run_id_display.cell(),
                    workflow_display.cell(),
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
    }

    Ok(())
}

pub fn prune_command(args: &RunsPruneArgs) -> Result<()> {
    let base = default_runs_base();
    prune_from(args, &base)
}

/// Parse a human duration string like "24h" or "7d" into a `chrono::Duration`.
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

pub fn prune_from(args: &RunsPruneArgs, base: &Path) -> Result<()> {
    let runs = scan_runs(base)?;
    let label_filters = parse_label_filters(&args.filter.label);
    let mut filtered = filter_runs(
        &runs,
        args.filter.before.as_deref(),
        args.filter.workflow.as_deref(),
        &label_filters,
        args.filter.orphans,
        StatusFilter::All,
    );

    // Determine if the user passed any explicit filters
    let has_explicit_filters =
        args.filter.before.is_some() || args.filter.workflow.is_some() || !label_filters.is_empty();

    // Apply staleness filter: default 24h when no explicit filters, or use --older-than
    let staleness_threshold = if let Some(dur) = args.older_than {
        Some(dur)
    } else if !has_explicit_filters {
        Some(chrono::Duration::hours(24))
    } else {
        None
    };

    if let Some(threshold) = staleness_threshold {
        let now = Utc::now();
        let cutoff = now - threshold;
        filtered.retain(|run| {
            // Exclude active runs
            if run.status.is_active() {
                return false;
            }
            // Use end_time if available, fall back to start_time
            let effective_time = run.end_time.or(run.start_time_dt);
            match effective_time {
                Some(t) => t < cutoff,
                None => false,
            }
        });
    }

    if filtered.is_empty() {
        eprintln!("No matching runs to prune.");
        return Ok(());
    }

    // Calculate total disk space
    let total_bytes: u64 = filtered.iter().map(|r| dir_size(&r.path)).sum();
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
    } else {
        for run in &filtered {
            debug!(run_id = %run.run_id, "would delete run (dry-run)");
            println!("would delete: {} ({})", run.dir_name, run.workflow_name);
        }
        eprintln!(
            "\n{} run(s) would be deleted ({} freed). Pass --yes to confirm.",
            filtered.len(),
            format_size(total_bytes)
        );
    }
    Ok(())
}

pub async fn remove_command(args: &RunsRemoveArgs) -> Result<()> {
    let base = default_runs_base();
    remove_from(args, &base).await
}

pub async fn remove_from(args: &RunsRemoveArgs, base: &Path) -> Result<()> {
    let mut had_errors = false;

    for identifier in &args.runs {
        let run = match resolve_run(base, identifier) {
            Ok(run) => run,
            Err(e) => {
                eprintln!("error: {identifier}: {e}");
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

        // Transition status to Removing (best-effort)
        write_run_status(&run.path, RunStatus::Removing, None);

        // Best-effort sandbox cleanup
        let sandbox_path = run.path.join("sandbox.json");
        if let Ok(record) = crate::sandbox_record::SandboxRecord::load(&sandbox_path) {
            if record.provider != "local" {
                match super::cp::reconnect(&record).await {
                    Ok(sandbox) => {
                        if let Err(e) = sandbox.cleanup().await {
                            warn!(run_id = %run.run_id, error = %e, "sandbox cleanup failed");
                        }
                    }
                    Err(e) => {
                        warn!(run_id = %run.run_id, error = %e, "sandbox reconnect failed");
                    }
                }
            }
        }

        // Delete the run directory
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
    use std::fs;

    fn make_run_dir(
        base: &Path,
        dir_name: &str,
        manifest: Option<serde_json::Value>,
        conclusion_json: Option<serde_json::Value>,
        status: Option<(RunStatus, Option<StatusReason>)>,
    ) -> PathBuf {
        let dir = base.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        if let Some(m) = manifest {
            fs::write(
                dir.join("manifest.json"),
                serde_json::to_string_pretty(&m).unwrap(),
            )
            .unwrap();
        }
        if let Some(c) = conclusion_json {
            fs::write(
                dir.join("conclusion.json"),
                serde_json::to_string_pretty(&c).unwrap(),
            )
            .unwrap();
        }
        if let Some((s, r)) = status {
            write_run_status(&dir, s, r);
        }
        dir
    }

    #[test]
    fn scan_runs_reads_manifest_and_final() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        make_run_dir(
            base,
            "20260101-ABC123",
            Some(serde_json::json!({
                "run_id": "abc123",
                "workflow_name": "my-pipeline",
                "goal": "test goal",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 2,
                "edge_count": 1,
                "labels": { "env": "prod" }
            })),
            Some(
                serde_json::json!({ "timestamp": "2026-01-01T12:01:00Z", "status": "success", "duration_ms": 60000 }),
            ),
            Some((RunStatus::Succeeded, Some(StatusReason::Completed))),
        );

        make_run_dir(base, "fabro-run-orphan", None, None, None);

        let runs = scan_runs(base).unwrap();
        assert_eq!(runs.len(), 2);

        let completed = runs.iter().find(|r| r.run_id == "abc123").unwrap();
        assert_eq!(completed.workflow_name, "my-pipeline");
        assert_eq!(completed.status, RunStatus::Succeeded);
        assert_eq!(completed.labels.get("env").unwrap(), "prod");
        assert!(!completed.is_orphan);

        let orphan = runs.iter().find(|r| r.is_orphan).unwrap();
        assert_eq!(orphan.workflow_name, "[no manifest]");
        assert_eq!(orphan.status, RunStatus::Dead);
    }

    #[test]
    fn scan_runs_detects_running_status() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        make_run_dir(
            base,
            "20260115-RUNNING1",
            Some(serde_json::json!({
                "run_id": "running-1",
                "workflow_name": "pipeline-a",
                "goal": "",
                "start_time": "2026-01-15T10:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            Some((RunStatus::Running, None)),
        );

        let runs = scan_runs(base).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Running);
    }

    #[test]
    fn scan_runs_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = scan_runs(tmp.path()).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn scan_runs_missing_dir() {
        let runs = scan_runs(Path::new("/tmp/nonexistent-arc-test-dir")).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn filter_runs_before() {
        let runs = vec![
            RunInfo {
                run_id: "old".into(),
                dir_name: "d1".into(),
                workflow_name: "p".into(),
                workflow_slug: None,
                status: RunStatus::Succeeded,
                status_reason: None,
                start_time: "2025-06-01T00:00:00Z".into(),
                labels: HashMap::new(),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d1"),
                goal: String::new(),
                is_orphan: false,
            },
            RunInfo {
                run_id: "new".into(),
                dir_name: "d2".into(),
                workflow_name: "p".into(),
                workflow_slug: None,
                status: RunStatus::Succeeded,
                status_reason: None,
                start_time: "2026-03-01T00:00:00Z".into(),
                labels: HashMap::new(),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d2"),
                goal: String::new(),
                is_orphan: false,
            },
        ];
        let filtered = filter_runs(
            &runs,
            Some("2026-01-01"),
            None,
            &[],
            false,
            StatusFilter::All,
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, "old");
    }

    #[test]
    fn filter_runs_workflow() {
        let runs = vec![
            RunInfo {
                run_id: "a".into(),
                dir_name: "d1".into(),
                workflow_name: "deploy-prod".into(),
                workflow_slug: None,
                status: RunStatus::Succeeded,
                status_reason: None,
                start_time: "2026-01-01T00:00:00Z".into(),
                labels: HashMap::new(),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d1"),
                goal: String::new(),
                is_orphan: false,
            },
            RunInfo {
                run_id: "b".into(),
                dir_name: "d2".into(),
                workflow_name: "test-suite".into(),
                workflow_slug: None,
                status: RunStatus::Succeeded,
                status_reason: None,
                start_time: "2026-01-01T00:00:00Z".into(),
                labels: HashMap::new(),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d2"),
                goal: String::new(),
                is_orphan: false,
            },
        ];
        let filtered = filter_runs(&runs, None, Some("deploy"), &[], false, StatusFilter::All);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, "a");
    }

    #[test]
    fn filter_runs_labels() {
        let runs = vec![
            RunInfo {
                run_id: "a".into(),
                dir_name: "d1".into(),
                workflow_name: "p".into(),
                workflow_slug: None,
                status: RunStatus::Succeeded,
                status_reason: None,
                start_time: "2026-01-01T00:00:00Z".into(),
                labels: HashMap::from([("env".into(), "prod".into())]),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d1"),
                goal: String::new(),
                is_orphan: false,
            },
            RunInfo {
                run_id: "b".into(),
                dir_name: "d2".into(),
                workflow_name: "p".into(),
                workflow_slug: None,
                status: RunStatus::Succeeded,
                status_reason: None,
                start_time: "2026-01-01T00:00:00Z".into(),
                labels: HashMap::from([("env".into(), "staging".into())]),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d2"),
                goal: String::new(),
                is_orphan: false,
            },
        ];
        let filtered = filter_runs(
            &runs,
            None,
            None,
            &[("env".to_string(), "prod".to_string())],
            false,
            StatusFilter::All,
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, "a");
    }

    #[test]
    fn filter_runs_orphans_excluded_by_default() {
        let runs = vec![RunInfo {
            run_id: "orphan".into(),
            dir_name: "d1".into(),
            workflow_name: "[no manifest]".into(),
            workflow_slug: None,
            status: RunStatus::Dead,
            status_reason: None,
            start_time: "".into(),
            labels: HashMap::new(),
            duration_ms: None,
            total_cost: None,
            host_repo_path: None,
            start_time_dt: None,
            end_time: None,
            path: PathBuf::from("/tmp/d1"),
            goal: String::new(),
            is_orphan: true,
        }];
        let filtered = filter_runs(&runs, None, None, &[], false, StatusFilter::All);
        assert!(filtered.is_empty());

        let filtered = filter_runs(&runs, None, None, &[], true, StatusFilter::All);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_runs_running_only() {
        let runs = vec![
            RunInfo {
                run_id: "running-1".into(),
                dir_name: "d1".into(),
                workflow_name: "p".into(),
                workflow_slug: None,
                status: RunStatus::Running,
                status_reason: None,
                start_time: "2026-01-01T00:00:00Z".into(),
                labels: HashMap::new(),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d1"),
                goal: String::new(),
                is_orphan: false,
            },
            RunInfo {
                run_id: "done-1".into(),
                dir_name: "d2".into(),
                workflow_name: "p".into(),
                workflow_slug: None,
                status: RunStatus::Succeeded,
                status_reason: None,
                start_time: "2026-01-01T00:00:00Z".into(),
                labels: HashMap::new(),
                duration_ms: None,
                total_cost: None,
                host_repo_path: None,
                start_time_dt: None,
                end_time: None,
                path: PathBuf::from("/tmp/d2"),
                goal: String::new(),
                is_orphan: false,
            },
        ];

        let filtered = filter_runs(&runs, None, None, &[], false, StatusFilter::RunningOnly);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, "running-1");

        let filtered = filter_runs(&runs, None, None, &[], false, StatusFilter::All);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn scan_runs_extracts_host_repo_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        make_run_dir(
            base,
            "20260101-ABC123",
            Some(serde_json::json!({
                "run_id": "abc123",
                "workflow_name": "my-pipeline",
                "goal": "test goal",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 2,
                "edge_count": 1,
                "host_repo_path": "/home/user/myproject"
            })),
            None,
            Some((RunStatus::Running, None)),
        );

        let runs = scan_runs(base).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].host_repo_path.as_deref(),
            Some("/home/user/myproject")
        );
    }

    #[test]
    fn prune_dry_run_preserves_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let dir = make_run_dir(
            base,
            "20250101-TOPRUNE",
            Some(serde_json::json!({
                "run_id": "to-prune",
                "workflow_name": "old-pipeline",
                "goal": "",
                "start_time": "2025-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            Some(
                serde_json::json!({ "timestamp": "2025-01-01T12:01:00Z", "status": "success", "duration_ms": 60000 }),
            ),
            None,
        );

        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: Some("2026-01-01".into()),
                workflow: None,
                label: Vec::new(),
                orphans: false,
            },
            older_than: None,
            yes: false,
        };

        prune_from(&args, base).unwrap();
        assert!(dir.exists(), "dry-run should preserve directory");
    }

    #[test]
    fn prune_with_yes_deletes_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let dir = make_run_dir(
            base,
            "20250101-TOPRUNE",
            Some(serde_json::json!({
                "run_id": "to-prune",
                "workflow_name": "old-pipeline",
                "goal": "",
                "start_time": "2025-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            Some(
                serde_json::json!({ "timestamp": "2025-01-01T12:01:00Z", "status": "success", "duration_ms": 60000 }),
            ),
            None,
        );

        // Also add a run that should NOT be pruned (too new)
        let keep_dir = make_run_dir(
            base,
            "20260301-KEEPTHIS",
            Some(serde_json::json!({
                "run_id": "keep-this",
                "workflow_name": "new-pipeline",
                "goal": "",
                "start_time": "2026-03-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            Some(
                serde_json::json!({ "timestamp": "2026-03-01T12:01:00Z", "status": "success", "duration_ms": 60000 }),
            ),
            None,
        );

        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: Some("2026-01-01".into()),
                workflow: None,
                label: Vec::new(),
                orphans: false,
            },
            older_than: None,
            yes: true,
        };

        prune_from(&args, base).unwrap();
        assert!(!dir.exists(), "--yes should delete matching directory");
        assert!(
            keep_dir.exists(),
            "non-matching directory should be preserved"
        );
    }

    #[test]
    fn prune_orphans_with_yes() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let orphan_dir = make_run_dir(base, "orphan-dir", None, None, None);

        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: None,
                workflow: None,
                label: Vec::new(),
                orphans: true,
            },
            older_than: Some(chrono::Duration::zero()),
            yes: true,
        };

        prune_from(&args, base).unwrap();
        assert!(!orphan_dir.exists(), "orphan directory should be deleted");
    }

    #[test]
    fn parse_label_filters_basic() {
        let args = vec!["env=prod".to_string()];
        let result = parse_label_filters(&args);
        assert_eq!(result, vec![("env".to_string(), "prod".to_string())]);
    }

    #[test]
    fn parse_label_filters_multiple() {
        let args = vec!["a=1".to_string(), "b=2".to_string()];
        let result = parse_label_filters(&args);
        assert_eq!(result.len(), 2);
        assert!(result.contains(&("a".to_string(), "1".to_string())));
        assert!(result.contains(&("b".to_string(), "2".to_string())));
    }

    #[test]
    fn parse_label_filters_value_with_equals() {
        let args = vec!["key=a=b".to_string()];
        let result = parse_label_filters(&args);
        assert_eq!(result, vec![("key".to_string(), "a=b".to_string())]);
    }

    #[test]
    fn parse_label_filters_skips_no_equals() {
        let args = vec!["nope".to_string(), "a=1".to_string()];
        let result = parse_label_filters(&args);
        assert_eq!(result, vec![("a".to_string(), "1".to_string())]);
    }

    #[test]
    fn parse_label_filters_empty() {
        let args: Vec<String> = vec![];
        let result = parse_label_filters(&args);
        assert!(result.is_empty());
    }

    #[test]
    fn dir_size_works() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // Create nested files with known sizes
        fs::write(base.join("a.txt"), "hello").unwrap(); // 5 bytes
        let sub = base.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("b.txt"), "world!").unwrap(); // 6 bytes

        assert_eq!(dir_size(base), 11);
    }

    #[test]
    fn df_reports_run_sizes() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let runs_base = data_dir.join("runs");
        fs::create_dir(&runs_base).unwrap();
        let logs_base = data_dir.join("logs");
        fs::create_dir(&logs_base).unwrap();

        // Running run
        make_run_dir(
            &runs_base,
            "20260308-RUNNING",
            Some(serde_json::json!({
                "run_id": "running-1",
                "workflow_name": "code-review",
                "goal": "",
                "start_time": "2026-03-08T10:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            Some((RunStatus::Running, None)),
        );
        // Add a file to give it size
        fs::write(
            runs_base.join("20260308-RUNNING").join("data.bin"),
            vec![0u8; 100],
        )
        .unwrap();

        // Completed run
        make_run_dir(
            &runs_base,
            "20260307-DONE",
            Some(serde_json::json!({
                "run_id": "done-1",
                "workflow_name": "deploy",
                "goal": "",
                "start_time": "2026-03-07T10:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": "2026-03-07T10:01:00Z",
                "status": "success",
                "duration_ms": 60000
            })),
            None,
        );
        fs::write(
            runs_base.join("20260307-DONE").join("data.bin"),
            vec![0u8; 200],
        )
        .unwrap();

        let args = DfArgs { verbose: false };
        // Should not panic
        df_from(&args, data_dir, &runs_base, &logs_base).unwrap();
    }

    #[test]
    fn df_reports_log_files() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let runs_base = data_dir.join("runs");
        fs::create_dir(&runs_base).unwrap();
        let logs_base = data_dir.join("logs");
        fs::create_dir(&logs_base).unwrap();

        fs::write(logs_base.join("cli-2026-03-08.log"), vec![0u8; 500]).unwrap();
        fs::write(logs_base.join("serve-2026-03-08.log"), vec![0u8; 300]).unwrap();

        let args = DfArgs { verbose: false };
        df_from(&args, data_dir, &runs_base, &logs_base).unwrap();
    }

    #[test]
    fn df_reports_database_files() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let runs_base = data_dir.join("runs");
        fs::create_dir(&runs_base).unwrap();
        let logs_base = data_dir.join("logs");
        fs::create_dir(&logs_base).unwrap();

        fs::write(data_dir.join("fabro.db"), vec![0u8; 1024]).unwrap();
        fs::write(data_dir.join("fabro.db-wal"), vec![0u8; 512]).unwrap();
        fs::write(data_dir.join("fabro.db-shm"), vec![0u8; 32]).unwrap();

        let args = DfArgs { verbose: false };
        df_from(&args, data_dir, &runs_base, &logs_base).unwrap();
    }

    #[test]
    fn find_run_by_prefix_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_run_by_prefix(dir.path(), "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn find_run_by_prefix_single_match() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("20260101-ABC123");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(
            run_dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": "abc123-full-id",
                "workflow_name": "test",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }))
            .unwrap(),
        )
        .unwrap();

        let result = find_run_by_prefix(dir.path(), "abc123").unwrap();
        assert_eq!(result, run_dir);
    }

    #[test]
    fn find_run_by_prefix_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let subdirs = [("d1", "abc-111"), ("d2", "abc-222")];
        for (subdir, run_id) in subdirs {
            let run_dir = dir.path().join(subdir);
            fs::create_dir_all(&run_dir).unwrap();
            fs::write(
                run_dir.join("manifest.json"),
                serde_json::to_string_pretty(&serde_json::json!({
                    "run_id": run_id,
                    "workflow_name": "test",
                    "goal": "",
                    "start_time": "2026-01-01T12:00:00Z",
                    "node_count": 1,
                    "edge_count": 0
                }))
                .unwrap(),
            )
            .unwrap();
        }

        let result = find_run_by_prefix(dir.path(), "abc");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Ambiguous"),
            "Should mention ambiguity"
        );
    }

    // === resolve_run tests ===

    #[test]
    fn resolve_run_by_run_id_prefix() {
        let dir = tempfile::tempdir().unwrap();
        make_run_dir(
            dir.path(),
            "20260101-ABC123",
            Some(serde_json::json!({
                "run_id": "abc123-full-id",
                "workflow_name": "deploy",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );

        let info = resolve_run(dir.path(), "abc123").unwrap();
        assert_eq!(info.run_id, "abc123-full-id");
    }

    #[test]
    fn resolve_run_falls_back_to_workflow_name() {
        let dir = tempfile::tempdir().unwrap();
        make_run_dir(
            dir.path(),
            "20260101-AAA111",
            Some(serde_json::json!({
                "run_id": "aaa111-old",
                "workflow_name": "deploy",
                "goal": "",
                "start_time": "2026-01-01T11:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );
        make_run_dir(
            dir.path(),
            "20260102-BBB222",
            Some(serde_json::json!({
                "run_id": "bbb222-new",
                "workflow_name": "deploy",
                "goal": "",
                "start_time": "2026-01-02T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );

        // "deploy" doesn't match any run ID prefix, so falls back to workflow name
        let info = resolve_run(dir.path(), "deploy").unwrap();
        // Should return the most recent one
        assert_eq!(info.run_id, "bbb222-new");
    }

    #[test]
    fn resolve_run_id_prefix_takes_priority_over_workflow_name() {
        let dir = tempfile::tempdir().unwrap();
        // run_id starts with "deploy" AND workflow is "deploy"
        make_run_dir(
            dir.path(),
            "20260101-DEPLOY",
            Some(serde_json::json!({
                "run_id": "deploy-run-1",
                "workflow_name": "other-workflow",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );
        make_run_dir(
            dir.path(),
            "20260102-ZZZ999",
            Some(serde_json::json!({
                "run_id": "zzz999-newer",
                "workflow_name": "deploy",
                "goal": "",
                "start_time": "2026-01-02T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );

        // "deploy" matches run_id prefix of first run — should prefer that over workflow name match
        let info = resolve_run(dir.path(), "deploy").unwrap();
        assert_eq!(info.run_id, "deploy-run-1");
    }

    #[test]
    fn resolve_run_errors_on_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_run(dir.path(), "nonexistent");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No run found"), "got: {msg}");
    }

    #[test]
    fn resolve_run_errors_on_ambiguous_prefix() {
        let dir = tempfile::tempdir().unwrap();
        make_run_dir(
            dir.path(),
            "20260101-D1",
            Some(serde_json::json!({
                "run_id": "abc-111",
                "workflow_name": "test",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );
        make_run_dir(
            dir.path(),
            "20260101-D2",
            Some(serde_json::json!({
                "run_id": "abc-222",
                "workflow_name": "test",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );

        let result = resolve_run(dir.path(), "abc");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Ambiguous"),
            "Should mention ambiguity"
        );
    }

    #[test]
    fn resolve_run_matches_slug_to_pascal_case_workflow_name() {
        let dir = tempfile::tempdir().unwrap();
        make_run_dir(
            dir.path(),
            "20260101-AAA111",
            Some(serde_json::json!({
                "run_id": "aaa111-full",
                "workflow_name": "LegacyTool",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );

        // Slug-style input should match PascalCase workflow name
        let info = resolve_run(dir.path(), "legacy-tool").unwrap();
        assert_eq!(info.run_id, "aaa111-full");
    }

    #[test]
    fn resolve_run_matches_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        make_run_dir(
            dir.path(),
            "20260101-AAA111",
            Some(serde_json::json!({
                "run_id": "aaa111-full",
                "workflow_name": "Smoke",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );

        let info = resolve_run(dir.path(), "smoke").unwrap();
        assert_eq!(info.run_id, "aaa111-full");
    }

    #[test]
    fn resolve_run_matches_by_workflow_slug() {
        let dir = tempfile::tempdir().unwrap();
        // Graph name is "Bar" but slug is "foo" — resolve_run("foo") should match via slug
        make_run_dir(
            dir.path(),
            "20260101-AAA111",
            Some(serde_json::json!({
                "run_id": "aaa111-full",
                "workflow_name": "Bar",
                "workflow_slug": "foo",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            None,
        );

        let info = resolve_run(dir.path(), "foo").unwrap();
        assert_eq!(info.run_id, "aaa111-full");
        assert_eq!(info.workflow_slug.as_deref(), Some("foo"));
    }

    // === Step 1: end_time tests ===

    #[test]
    fn scan_runs_populates_end_time() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        make_run_dir(
            base,
            "20260101-DONE",
            Some(serde_json::json!({
                "run_id": "done-1",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": "2026-01-01T12:05:00Z",
                "status": "success",
                "duration_ms": 300000
            })),
            Some((RunStatus::Succeeded, Some(StatusReason::Completed))),
        );

        let runs = scan_runs(base).unwrap();
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert!(run.end_time.is_some());
        assert_eq!(
            run.end_time.unwrap().to_rfc3339(),
            "2026-01-01T12:05:00+00:00"
        );
    }

    #[test]
    fn scan_runs_end_time_none_when_running() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        make_run_dir(
            base,
            "20260101-RUNNING",
            Some(serde_json::json!({
                "run_id": "running-1",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            Some((RunStatus::Running, None)),
        );

        let runs = scan_runs(base).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Running);
        assert!(runs[0].end_time.is_none());
    }

    // === Step 2: staleness heuristic tests ===

    #[test]
    fn prune_default_skips_recent_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // A run completed just now — should NOT be pruned by default
        let now = Utc::now();
        let recent_ts = now.to_rfc3339();
        let dir = make_run_dir(
            base,
            "20260309-RECENT",
            Some(serde_json::json!({
                "run_id": "recent-1",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": &recent_ts,
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": &recent_ts,
                "status": "success",
                "duration_ms": 1000
            })),
            None,
        );

        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: None,
                workflow: None,
                label: Vec::new(),
                orphans: false,
            },
            older_than: None,
            yes: false,
        };

        prune_from(&args, base).unwrap();
        assert!(dir.exists(), "recent run should not be pruned by default");
    }

    #[test]
    fn prune_default_skips_running_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let old_ts = (Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        let dir = make_run_dir(
            base,
            "20260307-RUNNING",
            Some(serde_json::json!({
                "run_id": "running-old",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": &old_ts,
                "node_count": 1,
                "edge_count": 0
            })),
            None,
            Some((RunStatus::Running, None)), // running
        );

        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: None,
                workflow: None,
                label: Vec::new(),
                orphans: false,
            },
            older_than: None,
            yes: false,
        };

        prune_from(&args, base).unwrap();
        assert!(dir.exists(), "running run should not be pruned");
    }

    #[test]
    fn prune_default_targets_stale_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let old_ts = (Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        let dir = make_run_dir(
            base,
            "20260307-STALE",
            Some(serde_json::json!({
                "run_id": "stale-1",
                "workflow_name": "old-pipeline",
                "goal": "",
                "start_time": &old_ts,
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": &old_ts,
                "status": "success",
                "duration_ms": 1000
            })),
            None,
        );

        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: None,
                workflow: None,
                label: Vec::new(),
                orphans: false,
            },
            older_than: None,
            yes: true,
        };

        prune_from(&args, base).unwrap();
        assert!(
            !dir.exists(),
            "stale run (48h old) should be pruned by default"
        );
    }

    #[test]
    fn prune_explicit_filter_bypasses_staleness() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // A run completed just now, but matched by --before
        let now = Utc::now();
        let recent_ts = now.to_rfc3339();
        let dir = make_run_dir(
            base,
            "20260309-RECENT",
            Some(serde_json::json!({
                "run_id": "recent-1",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": "2025-06-01T00:00:00Z",
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": &recent_ts,
                "status": "success",
                "duration_ms": 1000
            })),
            None,
        );

        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: Some("2026-01-01".into()),
                workflow: None,
                label: Vec::new(),
                orphans: false,
            },
            older_than: None,
            yes: true,
        };

        prune_from(&args, base).unwrap();
        assert!(
            !dir.exists(),
            "explicit --before should bypass staleness filter"
        );
    }

    // === Step 2: parse_duration tests ===

    #[test]
    fn parse_duration_hours() {
        let dur = parse_duration("24h").unwrap();
        assert_eq!(dur, chrono::Duration::hours(24));
    }

    #[test]
    fn parse_duration_days() {
        let dur = parse_duration("7d").unwrap();
        assert_eq!(dur, chrono::Duration::days(7));
    }

    #[test]
    fn parse_duration_invalid() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn parse_duration_rejects_negative() {
        assert!(parse_duration("-1d").is_err());
        assert!(parse_duration("-24h").is_err());
    }

    #[test]
    fn run_status_serializes_as_snake_case_string() {
        assert_eq!(
            serde_json::to_string(&RunStatus::Succeeded).unwrap(),
            "\"succeeded\""
        );
        assert_eq!(
            serde_json::to_string(&RunStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(serde_json::to_string(&RunStatus::Dead).unwrap(), "\"dead\"");
        assert_eq!(
            serde_json::to_string(&RunStatus::Failed).unwrap(),
            "\"failed\""
        );
    }

    // === Step 3: disk space reporting tests ===

    #[test]
    fn format_size_display() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(
            format_size(1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "1.5 GB"
        );
    }

    // === Step 4: custom threshold test ===

    #[test]
    fn prune_older_than_custom_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // Run completed 3 days ago — should be pruned with --older-than 2d
        let three_days_ago = (Utc::now() - chrono::Duration::days(3)).to_rfc3339();
        let dir_old = make_run_dir(
            base,
            "20260306-OLD",
            Some(serde_json::json!({
                "run_id": "old-1",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": &three_days_ago,
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": &three_days_ago,
                "status": "success",
                "duration_ms": 1000
            })),
            None,
        );

        // Run completed 1 day ago — should NOT be pruned with --older-than 2d
        let one_day_ago = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let dir_recent = make_run_dir(
            base,
            "20260308-RECENT",
            Some(serde_json::json!({
                "run_id": "recent-1",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": &one_day_ago,
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": &one_day_ago,
                "status": "success",
                "duration_ms": 1000
            })),
            None,
        );

        // Run completed 10 days ago — should be pruned with --older-than 7d
        let ten_days_ago = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        let dir_very_old = make_run_dir(
            base,
            "20260227-VERYOLD",
            Some(serde_json::json!({
                "run_id": "very-old-1",
                "workflow_name": "pipeline",
                "goal": "",
                "start_time": &ten_days_ago,
                "node_count": 1,
                "edge_count": 0
            })),
            Some(serde_json::json!({
                "timestamp": &ten_days_ago,
                "status": "success",
                "duration_ms": 1000
            })),
            None,
        );

        // With --older-than 7d, only the 10-day-old run should be pruned
        let args = RunsPruneArgs {
            filter: RunFilterArgs {
                before: None,
                workflow: None,
                label: Vec::new(),
                orphans: false,
            },
            older_than: Some(chrono::Duration::days(7)),
            yes: true,
        };

        prune_from(&args, base).unwrap();
        assert!(
            dir_old.exists(),
            "3-day-old run should survive 7d threshold"
        );
        assert!(
            dir_recent.exists(),
            "1-day-old run should survive 7d threshold"
        );
        assert!(
            !dir_very_old.exists(),
            "10-day-old run should be pruned with 7d threshold"
        );
    }

    // === status.json tests ===

    #[test]
    fn read_status_from_status_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_run_status(dir, RunStatus::Running, None);
        let si = read_status(dir);
        assert_eq!(si.status, RunStatus::Running);
    }

    #[test]
    fn read_status_succeeded_with_conclusion_has_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_run_status(dir, RunStatus::Succeeded, Some(StatusReason::Completed));
        fs::write(
            dir.join("conclusion.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "timestamp": "2026-01-01T12:01:00Z",
                "status": "success",
                "duration_ms": 60000
            }))
            .unwrap(),
        )
        .unwrap();
        let si = read_status(dir);
        assert_eq!(si.status, RunStatus::Succeeded);
        assert_eq!(si.duration_ms, Some(60000));
    }

    #[test]
    fn read_status_no_status_json_is_dead() {
        let tmp = tempfile::tempdir().unwrap();
        let si = read_status(tmp.path());
        assert_eq!(si.status, RunStatus::Dead);
    }

    #[test]
    fn scan_runs_status_json_without_manifest_is_not_orphan() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let dir = base.join("20260301-STARTING");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("id.txt"), "starting-run-id").unwrap();
        write_run_status(
            &dir,
            RunStatus::Starting,
            Some(StatusReason::SandboxInitializing),
        );

        let runs = scan_runs(base).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].is_orphan);
        assert_eq!(runs[0].status, RunStatus::Starting);
        assert_eq!(runs[0].workflow_name, "[starting]");
    }

    #[test]
    fn scan_runs_no_status_json_no_manifest_is_orphan() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let dir = base.join("20260301-ORPHAN");
        fs::create_dir_all(&dir).unwrap();

        let runs = scan_runs(base).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].is_orphan);
        assert_eq!(runs[0].status, RunStatus::Dead);
    }

    #[test]
    fn filter_runs_running_only_includes_starting_runs() {
        let runs = vec![RunInfo {
            run_id: "starting-1".into(),
            dir_name: "d1".into(),
            workflow_name: "[starting]".into(),
            workflow_slug: None,
            status: RunStatus::Starting,
            status_reason: Some(StatusReason::SandboxInitializing),
            start_time: "2026-01-01T00:00:00Z".into(),
            labels: HashMap::new(),
            duration_ms: None,
            total_cost: None,
            host_repo_path: None,
            start_time_dt: None,
            end_time: None,
            path: PathBuf::from("/tmp/d1"),
            goal: String::new(),
            is_orphan: false,
        }];

        let filtered = filter_runs(&runs, None, None, &[], false, StatusFilter::RunningOnly);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, "starting-1");
    }

    #[test]
    fn truncate_goal_short_string_unchanged() {
        assert_eq!(truncate_goal("short", 50), "short");
    }

    #[test]
    fn truncate_goal_long_string_truncated() {
        let long = "a]".repeat(30); // 60 chars
        let result = truncate_goal(&long, 50);
        assert_eq!(result.chars().count(), 50);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_goal_multibyte_safe() {
        let emoji_str = "Hello \u{1F600} world \u{1F600} test \u{1F600} more text here padding";
        // Should not panic
        let result = truncate_goal(emoji_str, 15);
        assert_eq!(result.chars().count(), 15);
        assert!(result.ends_with("..."));
    }

    fn make_succeeded_run(base: &Path, run_id: &str) -> PathBuf {
        make_run_dir(
            base,
            &format!("20260101-{run_id}"),
            Some(serde_json::json!({
                "run_id": run_id,
                "workflow_name": "test-wf",
                "goal": "test",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0,
                "labels": {}
            })),
            None,
            Some((RunStatus::Succeeded, Some(StatusReason::Completed))),
        )
    }

    fn make_running_run(base: &Path, run_id: &str) -> PathBuf {
        make_run_dir(
            base,
            &format!("20260101-{run_id}"),
            Some(serde_json::json!({
                "run_id": run_id,
                "workflow_name": "test-wf",
                "goal": "test",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0,
                "labels": {}
            })),
            None,
            Some((RunStatus::Running, None)),
        )
    }

    #[tokio::test]
    async fn test_remove_succeeded_run() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let dir = make_succeeded_run(base, "RUN1");
        assert!(dir.exists());

        let args = RunsRemoveArgs {
            runs: vec!["RUN1".to_string()],
            force: false,
        };
        remove_from(&args, base).await.unwrap();
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn test_remove_active_run_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let dir = make_running_run(base, "ACTIVE1");
        assert!(dir.exists());

        let args = RunsRemoveArgs {
            runs: vec!["ACTIVE1".to_string()],
            force: false,
        };
        let result = remove_from(&args, base).await;
        assert!(result.is_err());
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn test_remove_active_run_forced() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let dir = make_running_run(base, "ACTIVE2");
        assert!(dir.exists());

        let args = RunsRemoveArgs {
            runs: vec!["ACTIVE2".to_string()],
            force: true,
        };
        remove_from(&args, base).await.unwrap();
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn test_remove_unknown_run() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        fs::create_dir_all(base).unwrap();

        let args = RunsRemoveArgs {
            runs: vec!["NONEXISTENT".to_string()],
            force: false,
        };
        let result = remove_from(&args, base).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_remove_multiple_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let dir1 = make_succeeded_run(base, "MULTI1");
        let dir2 = make_succeeded_run(base, "MULTI2");
        assert!(dir1.exists());
        assert!(dir2.exists());

        let args = RunsRemoveArgs {
            runs: vec!["MULTI1".to_string(), "MULTI2".to_string()],
            force: false,
        };
        remove_from(&args, base).await.unwrap();
        assert!(!dir1.exists());
        assert!(!dir2.exists());
    }
}
