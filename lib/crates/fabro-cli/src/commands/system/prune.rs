use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use fabro_api::types;
use fabro_util::printer::Printer;
use tracing::{debug, info};

use crate::args::RunsPruneArgs;
use crate::command_context::CommandContext;
use crate::shared::{format_size, print_json_pretty};

pub(super) async fn prune_command(args: &RunsPruneArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_connection(&args.connection)?;
    let printer = ctx.printer();
    let server = ctx.server().await?;
    let response = server
        .prune_runs(types::PruneRunsRequest {
            before:     args.filter.before.clone(),
            dry_run:    !args.yes,
            labels:     parse_label_filters(&args.filter.label),
            older_than: args.older_than.map(format_duration),
            orphans:    args.filter.orphans,
            workflow:   args.filter.workflow.clone(),
        })
        .await?;
    prune_from(&response, ctx.json_output(), printer)
}

pub(crate) fn parse_duration(s: &str) -> Result<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration string");
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: i64 = num_str
        .parse()
        .with_context(|| format!("invalid duration: {s}"))?;
    match unit {
        "h" => Ok(chrono::Duration::hours(num)),
        "d" => Ok(chrono::Duration::days(num)),
        _ => bail!("invalid duration unit '{unit}' in '{s}' (expected 'h' or 'd')"),
    }
}

fn prune_from(
    response: &types::PruneRunsResponse,
    json_output: bool,
    printer: Printer,
) -> Result<()> {
    let total_count = response.total_count.unwrap_or_default();
    let total_size_bytes = response.total_size_bytes.unwrap_or_default();

    info!(
        count = total_count,
        bytes = total_size_bytes,
        dry_run = response.dry_run.unwrap_or(true),
        "pruning runs"
    );

    if json_output {
        print_json_pretty(response)?;
        return Ok(());
    }

    if total_count == 0 {
        fabro_util::printerr!(printer, "No matching runs to prune.");
        return Ok(());
    }

    if response.dry_run.unwrap_or(true) {
        for run in response.runs.as_deref().unwrap_or(&[]) {
            debug!(
                run_id = run.run_id.as_deref().unwrap_or("-"),
                "would delete run (dry-run)"
            );
            fabro_util::printout!(
                printer,
                "would delete: {} ({})",
                run.dir_name.as_deref().unwrap_or("-"),
                run.workflow_name.as_deref().unwrap_or("-")
            );
        }
        fabro_util::printerr!(
            printer,
            "\n{} run(s) would be deleted ({} freed). Pass --yes to confirm.",
            total_count,
            format_size(as_u64(total_size_bytes))
        );
        return Ok(());
    }

    fabro_util::printerr!(
        printer,
        "{} run(s) deleted ({} freed).",
        response.deleted_count.unwrap_or(total_count),
        format_size(as_u64(response.freed_bytes.unwrap_or(total_size_bytes)))
    );
    Ok(())
}

fn parse_label_filters(label_args: &[String]) -> HashMap<String, String> {
    label_args
        .iter()
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn format_duration(duration: chrono::Duration) -> String {
    if duration.num_hours() % 24 == 0 {
        format!("{}d", duration.num_days())
    } else {
        format!("{}h", duration.num_hours())
    }
}

fn as_u64(value: i64) -> u64 {
    value.try_into().unwrap_or_default()
}
