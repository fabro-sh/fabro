use anyhow::Result;
use chrono::{DateTime, Utc};
use cli_table::format::{Border, Justify, Separator};
use cli_table::{Cell, CellStruct, Style, Table};
use fabro_api::types;
use fabro_types::settings::cli::OutputFormat;

use crate::args::DfArgs;
use crate::command_context::CommandContext;
use crate::shared::{format_size, print_json_pretty};

pub(super) async fn df_command(args: &DfArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_connection(&args.connection)?;
    let server = ctx.server().await?;
    let json = ctx.user_settings().cli.output.format == OutputFormat::Json;

    let (output, storage_dir) = if json {
        (server.get_system_disk_usage(args.verbose).await?, None)
    } else {
        let (output, info) = tokio::try_join!(
            server.get_system_disk_usage(args.verbose),
            server.get_system_info(),
        )?;
        (output, info.storage_dir)
    };

    df_from(&output, storage_dir.as_deref(), json)
}

#[allow(
    clippy::print_stdout,
    reason = "The disk-usage report belongs on stdout for piping."
)]
fn df_from(
    output: &types::DiskUsageResponse,
    storage_dir: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let runs_summary = output
        .summary
        .iter()
        .find(|row| row.type_.as_deref() == Some("runs"));
    let logs_summary = output
        .summary
        .iter()
        .find(|row| row.type_.as_deref() == Some("logs"));

    let run_count = runs_summary.and_then(|row| row.count).map_or(0, as_u64);
    let active_count = runs_summary.and_then(|row| row.active).map_or(0, as_u64);
    let total_run_size = runs_summary
        .and_then(|row| row.size_bytes)
        .map_or(0, as_u64);
    let reclaimable_run_size = runs_summary
        .and_then(|row| row.reclaimable_bytes)
        .map_or(0, as_u64);

    let log_count = logs_summary.and_then(|row| row.count).map_or(0, as_u64);
    let total_log_size = logs_summary
        .and_then(|row| row.size_bytes)
        .map_or(0, as_u64);

    let run_reclaim_pct = if total_run_size > 0 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "The computed percentage is explicitly bounded to the 0..=100 range."
        )]
        // f64-to-integer: percentage is 0-100
        {
            (reclaimable_run_size as f64 / total_run_size as f64 * 100.0) as u64
        }
    } else {
        0
    };
    let log_reclaim_pct = if total_log_size > 0 { 100 } else { 0 };

    if json_output {
        print_json_pretty(output)?;
        return Ok(());
    }

    let use_color = console::colors_enabled();
    let color_choice = if use_color {
        cli_table::ColorChoice::Auto
    } else {
        cli_table::ColorChoice::Never
    };

    let summary_title = vec![
        "TYPE".cell().bold(use_color),
        "COUNT".cell().bold(use_color).justify(Justify::Right),
        "ACTIVE".cell().bold(use_color).justify(Justify::Right),
        "SIZE".cell().bold(use_color).justify(Justify::Right),
        "RECLAIMABLE".cell().bold(use_color).justify(Justify::Right),
    ];
    let summary_rows: Vec<Vec<CellStruct>> = vec![
        vec![
            "Runs".cell(),
            run_count.cell().justify(Justify::Right),
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
    ];
    let summary_table = summary_rows
        .table()
        .title(summary_title)
        .color_choice(color_choice)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    println!("{}", summary_table.display()?);

    if let Some(storage_dir) = storage_dir {
        println!();
        println!("Data directory: {storage_dir}");
    }

    let Some(run_rows) = output.runs.as_ref() else {
        return Ok(());
    };

    println!();
    let verbose_title = vec![
        "RUN ID".cell().bold(use_color),
        "WORKFLOW".cell().bold(use_color),
        "STATUS".cell().bold(use_color),
        "AGE".cell().bold(use_color).justify(Justify::Right),
        "SIZE".cell().bold(use_color).justify(Justify::Right),
    ];

    let now = Utc::now();
    let verbose_rows: Vec<Vec<CellStruct>> = run_rows
        .iter()
        .map(|detail| {
            let age = detail
                .start_time
                .as_deref()
                .and_then(parse_start_time)
                .map_or_else(
                    || "-".to_string(),
                    |dt| {
                        let dur = now.signed_duration_since(dt);
                        if dur.num_days() > 0 {
                            format!("{}d", dur.num_days())
                        } else if dur.num_hours() > 0 {
                            format!("{}h", dur.num_hours())
                        } else {
                            format!("{}m", dur.num_minutes().max(1))
                        }
                    },
                );
            let size = detail.size_bytes.map_or(0, as_u64);
            let size_display = if detail.reclaimable.unwrap_or(false) {
                format!("{} *", format_size(size))
            } else {
                format_size(size)
            };
            vec![
                short_run_id(detail.run_id.as_deref().unwrap_or("-")).cell(),
                truncate_str(detail.workflow_name.as_deref().unwrap_or("-"), 16).cell(),
                detail.status.as_deref().unwrap_or("-").cell(),
                age.cell().justify(Justify::Right),
                size_display.cell().justify(Justify::Right),
            ]
        })
        .collect();
    let verbose_table = verbose_rows
        .table()
        .title(verbose_title)
        .color_choice(color_choice)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    println!("{}", verbose_table.display()?);
    println!();
    println!("* = reclaimable");

    Ok(())
}

fn short_run_id(id: &str) -> &str {
    if id.len() > 12 { &id[..12] } else { id }
}

fn as_u64(value: i64) -> u64 {
    value.try_into().unwrap_or_default()
}

fn parse_start_time(value: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_len - 3).collect();
    format!("{truncated}...")
}
