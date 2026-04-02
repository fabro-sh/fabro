use std::path::Path;
use std::thread;
use std::time::Duration;

use fabro_server::bind::Bind;

use super::record;

pub(crate) fn execute(storage_dir: &Path, timeout: Duration) {
    let Some(record) = record::active_server_record(storage_dir) else {
        eprintln!("Server is not running");
        std::process::exit(1);
    };

    fabro_proc::sigterm(record.pid);

    // Poll for process exit
    let poll_interval = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < timeout {
        if !fabro_proc::process_alive(record.pid) {
            break;
        }
        thread::sleep(poll_interval);
        elapsed += poll_interval;
    }

    // Escalate to SIGKILL if still alive
    if fabro_proc::process_alive(record.pid) {
        fabro_proc::sigkill(record.pid);
        // Brief wait for SIGKILL to take effect
        thread::sleep(Duration::from_millis(100));
    }

    // Clean up record file
    let record_path = record::server_record_path(storage_dir);
    record::remove_server_record(&record_path);

    // Clean up Unix socket file if applicable
    if let Bind::Unix(ref path) = record.bind {
        let _ = std::fs::remove_file(path);
    }

    eprintln!("Server stopped");
}
