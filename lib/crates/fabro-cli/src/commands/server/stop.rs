use std::path::Path;
use std::time::Duration;

use fabro_server::bind::Bind;
use fabro_util::printer::Printer;
use tokio::time;

use super::record;

pub(crate) async fn execute(storage_dir: &Path, timeout: Duration, printer: Printer) {
    let Some(active) = record::active_server_record_details(storage_dir) else {
        fabro_util::printerr!(printer, "Server is not running");
        std::process::exit(1);
    };
    let record = active.record;

    fabro_proc::sigterm(record.pid);

    let poll_interval = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < timeout {
        if !fabro_proc::process_alive(record.pid) {
            break;
        }
        time::sleep(poll_interval).await;
        elapsed += poll_interval;
    }

    if fabro_proc::process_alive(record.pid) {
        fabro_proc::sigkill(record.pid);
        time::sleep(Duration::from_millis(100)).await;
    }

    record::remove_server_record(&active.record_path);

    if let Bind::Unix(ref path) = record.bind {
        let _ = std::fs::remove_file(path);
    }

    fabro_util::printerr!(printer, "Server stopped");
}
