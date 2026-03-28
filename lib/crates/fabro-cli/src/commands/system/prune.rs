use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use fabro_config::FabroSettingsExt;
use tracing::{debug, info};

use crate::args::RunsPruneArgs;
use crate::shared::format_size;

pub fn prune_command(args: &RunsPruneArgs) -> Result<()> {
    let cli_config = crate::cli_config::load_cli_settings(None)?;
    let base = fabro_workflows::run_lookup::runs_base(&cli_config.storage_dir());
    prune_from(args, &base)
}

pub(crate) fn parse_duration(s: &str) -> Result<chrono::Duration> {
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

fn parse_label_filters(label_args: &[String]) -> Vec<(String, String)> {
    label_args
        .iter()
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
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
