use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use fabro_config::RuntimeDirectory;
use fabro_config::daemon::ServerDaemon;
use fabro_util::printer::Printer;

pub(crate) fn execute(storage_dir: &Path, json: bool, printer: Printer) -> Result<()> {
    let runtime_directory = RuntimeDirectory::new(storage_dir);
    let Some(daemon) = ServerDaemon::load_running(&runtime_directory)? else {
        if json {
            fabro_util::printout!(printer, r#"{{"status":"stopped"}}"#);
        } else {
            fabro_util::printerr!(printer, "Server is not running");
        }
        std::process::exit(1);
    };

    if json {
        let uptime_seconds = (Utc::now() - daemon.started_at).num_seconds().max(0);
        let output = serde_json::json!({
            "status": "running",
            "pid": daemon.pid,
            "bind": daemon.bind.to_string(),
            "started_at": daemon.started_at.to_rfc3339(),
            "uptime_seconds": uptime_seconds,
        });
        fabro_util::printout!(printer, "{}", serde_json::to_string_pretty(&output)?);
    } else {
        let uptime = format_uptime(Utc::now() - daemon.started_at);
        fabro_util::printerr!(
            printer,
            "Server running (pid {}) on {}, started {} ago",
            daemon.pid,
            daemon.bind,
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
