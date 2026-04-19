use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use fabro_util::printer::Printer;

use super::record;

pub(crate) fn execute(storage_dir: &Path, json: bool, printer: Printer) -> Result<()> {
    let Some(record) = record::active_server_record(storage_dir)? else {
        if json {
            fabro_util::printout!(printer, r#"{{"status":"stopped"}}"#);
        } else {
            fabro_util::printerr!(printer, "Server is not running");
        }
        std::process::exit(1);
    };

    if json {
        let uptime_seconds = (Utc::now() - record.started_at).num_seconds().max(0);
        let output = serde_json::json!({
            "status": "running",
            "pid": record.pid,
            "bind": record.bind.to_string(),
            "started_at": record.started_at.to_rfc3339(),
            "uptime_seconds": uptime_seconds,
        });
        fabro_util::printout!(printer, "{}", serde_json::to_string_pretty(&output)?);
    } else {
        let uptime = format_uptime(Utc::now() - record.started_at);
        fabro_util::printerr!(
            printer,
            "Server running (pid {}) on {}, started {} ago",
            record.pid,
            record.bind,
            uptime
        );
    }

    Ok(())
}

fn format_uptime(duration: chrono::Duration) -> String {
    let total_seconds = duration.num_seconds().max(0);
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}
