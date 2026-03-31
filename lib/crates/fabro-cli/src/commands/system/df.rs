use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use cli_table::format::{Border, Justify, Separator};
use cli_table::{Cell, CellStruct, Style, Table};
use fabro_config::FabroSettingsExt;
use serde::Serialize;

use fabro_workflow::run_lookup::{logs_base, runs_base, scan_runs_combined};
use fabro_workflow::run_status::RunStatus;

use crate::args::{DfArgs, GlobalArgs};
use crate::shared::{format_size, print_json_pretty};
use crate::store;
use crate::user_config::load_user_settings_with_globals;

#[derive(Serialize)]
struct SummaryRow {
    r#type: String,
    count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<u64>,
    size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reclaimable_bytes: Option<u64>,
}

#[derive(Serialize)]
struct RunSizeRow {
    run_id: String,
    workflow_name: String,
    status: RunStatus,
    start_time: String,
    size_bytes: u64,
    reclaimable: bool,
}

#[derive(Serialize)]
struct DfOutput {
    summary: Vec<SummaryRow>,
    total_size_bytes: u64,
    total_reclaimable_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    runs: Option<Vec<RunSizeRow>>,
}

pub(super) async fn df_command(args: &DfArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let data_dir = cli_settings.storage_dir();
    let runs_base_dir = runs_base(&data_dir);
    let logs_base_dir = logs_base(&data_dir);
    let store = store::build_store(&data_dir)?;
    df_from(
        args,
        store.as_ref(),
        &data_dir,
        &runs_base_dir,
        &logs_base_dir,
        globals,
    )
    .await
}

#[allow(clippy::print_stdout)]
async fn df_from(
    args: &DfArgs,
    store: &dyn fabro_store::Store,
    data_dir: &Path,
    runs_base: &Path,
    logs_base: &Path,
    globals: &GlobalArgs,
) -> Result<()> {
    struct RunSizeInfo {
        run_id: String,
        workflow_name: String,
        status: RunStatus,
        start_time: String,
        start_time_dt: Option<DateTime<Utc>>,
        size: u64,
    }

    let runs = scan_runs_combined(store, runs_base).await?;
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
                run_id: run.run_id.to_string(),
                workflow_name: run.workflow_name.clone(),
                status: run.status,
                start_time: run.start_time.clone(),
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

    if globals.json {
        let summary = vec![
            SummaryRow {
                r#type: "runs".to_string(),
                count: runs.len().try_into().unwrap(),
                active: Some(active_count),
                size_bytes: total_run_size,
                reclaimable_bytes: Some(reclaimable_run_size),
            },
            SummaryRow {
                r#type: "logs".to_string(),
                count: log_count,
                active: None,
                size_bytes: total_log_size,
                reclaimable_bytes: Some(total_log_size),
            },
            SummaryRow {
                r#type: "databases".to_string(),
                count: db_count,
                active: None,
                size_bytes: total_db_size,
                reclaimable_bytes: Some(0),
            },
        ];
        let runs = args.verbose.then(|| {
            run_details
                .iter()
                .map(|detail| RunSizeRow {
                    run_id: detail.run_id.clone(),
                    workflow_name: detail.workflow_name.clone(),
                    status: detail.status,
                    start_time: detail.start_time.clone(),
                    size_bytes: detail.size,
                    reclaimable: !detail.status.is_active(),
                })
                .collect::<Vec<_>>()
        });
        print_json_pretty(&DfOutput {
            summary,
            total_size_bytes: total_run_size + total_log_size + total_db_size,
            total_reclaimable_bytes: reclaimable_run_size + total_log_size,
            runs,
        })?;
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
        .color_choice(color_choice)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    println!("{}", summary_table.display()?);

    println!();
    println!("Data directory: {}", data_dir.display());

    if !args.verbose {
        return Ok(());
    }

    println!();
    let verbose_title = vec![
        "RUN ID".cell().bold(use_color),
        "WORKFLOW".cell().bold(use_color),
        "STATUS".cell().bold(use_color),
        "AGE".cell().bold(use_color).justify(Justify::Right),
        "SIZE".cell().bold(use_color).justify(Justify::Right),
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
