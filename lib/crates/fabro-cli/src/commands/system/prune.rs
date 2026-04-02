use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fabro_store::Store;
use serde::Serialize;
use tracing::{debug, info};

use fabro_workflow::run_lookup::{StatusFilter, filter_runs, runs_base, scan_runs_combined};

use crate::args::{GlobalArgs, RunsPruneArgs};
use crate::commands::runs::rm::remove_run_with_cleanup;
use crate::shared::{format_size, print_json_pretty};
use crate::store;
use crate::user_config::load_user_settings_with_globals;

#[derive(Serialize)]
struct PruneRunRow {
    run_id: String,
    dir_name: String,
    workflow_name: String,
    size_bytes: u64,
}

pub(super) async fn prune_command(args: &RunsPruneArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    prune_from(args, store.as_ref(), &base, globals).await
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
        "h" => Ok(chrono::Duration::hours(i64::try_from(num).unwrap())),
        "d" => Ok(chrono::Duration::days(i64::try_from(num).unwrap())),
        _ => bail!("invalid duration unit '{unit}' in '{s}' (expected 'h' or 'd')"),
    }
}

async fn prune_from(
    args: &RunsPruneArgs,
    store: &dyn Store,
    base: &Path,
    globals: &GlobalArgs,
) -> Result<()> {
    let runs = scan_runs_combined(store, base).await?;
    let label_filters = parse_label_filters(&args.filter.label);
    let mut filtered = filter_runs(
        &runs,
        args.filter.before.as_deref(),
        args.filter.workflow.as_deref(),
        &label_filters,
        args.filter.orphans,
        StatusFilter::All,
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
            run.end_time
                .or(run.start_time_dt)
                .is_some_and(|time| time < cutoff)
        });
    }

    filtered.retain(|run| !run.status.is_active());

    if filtered.is_empty() {
        if globals.json {
            if args.yes {
                print_json_pretty(&serde_json::json!({
                    "dry_run": false,
                    "deleted_count": 0,
                    "freed_bytes": 0,
                }))?;
            } else {
                print_json_pretty(&serde_json::json!({
                    "dry_run": true,
                    "runs": Vec::<PruneRunRow>::new(),
                    "total_count": 0,
                    "total_size_bytes": 0,
                }))?;
            }
        } else {
            eprintln!("No matching runs to prune.");
        }
        return Ok(());
    }

    let rows: Vec<PruneRunRow> = filtered
        .iter()
        .map(|run| PruneRunRow {
            run_id: run.run_id.to_string(),
            dir_name: run.dir_name.clone(),
            workflow_name: run.workflow_name.clone(),
            size_bytes: dir_size(&run.path),
        })
        .collect();
    let total_bytes: u64 = rows.iter().map(|row| row.size_bytes).sum();
    info!(count = filtered.len(), bytes = total_bytes, "pruning runs");

    if args.yes {
        for run in &filtered {
            info!(run_id = %run.run_id, path = %run.path.display(), "deleting run");
            remove_run_with_cleanup(store, run).await?;
        }
        if globals.json {
            print_json_pretty(&serde_json::json!({
                "dry_run": false,
                "deleted_count": filtered.len(),
                "freed_bytes": total_bytes,
            }))?;
        } else {
            eprintln!(
                "{} run(s) deleted ({} freed).",
                filtered.len(),
                format_size(total_bytes)
            );
        }
        return Ok(());
    }

    if globals.json {
        print_json_pretty(&serde_json::json!({
            "dry_run": true,
            "runs": rows,
            "total_count": filtered.len(),
            "total_size_bytes": total_bytes,
        }))?;
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
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum()
}
