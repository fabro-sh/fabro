use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use cli_table::format::{Border, Justify, Separator};
use cli_table::{Cell, CellStruct, Style, Table, print_stdout};
use fabro_config::FabroSettingsExt;

use fabro_workflows::run_lookup::{logs_base, runs_base, scan_runs};
use fabro_workflows::run_status::RunStatus;

use crate::args::DfArgs;
use crate::cli_config::load_cli_settings;
use crate::shared::format_size;

pub(super) fn df_command(args: &DfArgs) -> Result<()> {
    let cli_config = load_cli_settings(None)?;
    let data_dir = cli_config.storage_dir();
    let runs_base_dir = runs_base(&data_dir);
    let logs_base_dir = logs_base(&data_dir);
    df_from(args, &data_dir, &runs_base_dir, &logs_base_dir)
}

fn df_from(args: &DfArgs, data_dir: &Path, runs_base: &Path, logs_base: &Path) -> Result<()> {
    struct RunSizeInfo {
        run_id: String,
        workflow_name: String,
        status: RunStatus,
        start_time_dt: Option<DateTime<Utc>>,
        size: u64,
    }

    let runs = scan_runs(runs_base)?;
    let mut active_count = 0u64;
    let mut total_run_size = 0u64;
    let mut reclaimable_run_size = 0u64;

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
            if std::path::Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("db"))
                || name.ends_with(".db-wal")
                || name.ends_with(".db-shm")
            {
                if let Ok(meta) = path.metadata() {
                    db_count += 1;
                    total_db_size += meta.len();
                }
            }
        }
    }

    let run_reclaim_pct = if total_run_size > 0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // f64-to-integer: percentage is 0-100
        {
            (reclaimable_run_size as f64 / total_run_size as f64 * 100.0) as u64
        }
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

fn short_run_id(id: &str) -> &str {
    if id.len() > 12 { &id[..12] } else { id }
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
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum()
}
