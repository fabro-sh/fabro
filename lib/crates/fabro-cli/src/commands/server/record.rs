use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fabro_server::bind::Bind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ServerRecord {
    pub pid: u32,
    pub bind: Bind,
    pub log_path: PathBuf,
    pub started_at: DateTime<Utc>,
}

pub(crate) fn server_record_path(storage_dir: &Path) -> PathBuf {
    storage_dir.join("server.json")
}

pub(crate) fn server_lock_path(storage_dir: &Path) -> PathBuf {
    storage_dir.join("server.lock")
}

pub(crate) fn server_log_path(storage_dir: &Path) -> PathBuf {
    storage_dir.join("server.log")
}

pub(crate) fn write_server_record(path: &Path, record: &ServerRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(record)?)
        .with_context(|| format!("Failed to write server metadata to {}", path.display()))
}

pub(crate) fn read_server_record(path: &Path) -> Option<ServerRecord> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(crate) fn remove_server_record(path: &Path) {
    let _ = std::fs::remove_file(path);
}

pub(crate) fn server_record_is_running(record: &ServerRecord) -> bool {
    fabro_proc::process_alive(record.pid) && server_process_matches(record)
}

pub(crate) fn active_server_record(storage_dir: &Path) -> Option<ServerRecord> {
    let path = server_record_path(storage_dir);
    let record = read_server_record(&path)?;
    if server_record_is_running(&record) {
        Some(record)
    } else {
        remove_server_record(&path);
        None
    }
}

#[cfg(unix)]
fn server_process_matches(record: &ServerRecord) -> bool {
    let output = match std::process::Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &record.pid.to_string()])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    let command = String::from_utf8_lossy(&output.stdout);
    command.contains("fabro") && command.contains("server")
}

#[cfg(not(unix))]
fn server_process_matches(_record: &ServerRecord) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record(bind: Bind) -> ServerRecord {
        ServerRecord {
            pid: std::process::id(),
            bind,
            log_path: PathBuf::from("/tmp/server.log"),
            started_at: Utc::now(),
        }
    }

    #[test]
    fn write_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = server_record_path(dir.path());
        let record = test_record(Bind::Tcp("127.0.0.1:3000".parse().unwrap()));
        write_server_record(&path, &record).unwrap();

        let loaded = read_server_record(&path).unwrap();
        assert_eq!(loaded.pid, record.pid);
        assert_eq!(loaded.bind, record.bind);
    }

    #[test]
    fn active_server_record_returns_none_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(active_server_record(dir.path()).is_none());
    }

    #[test]
    fn active_server_record_cleans_stale_dead_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = server_record_path(dir.path());
        let mut record = test_record(Bind::Tcp("127.0.0.1:3000".parse().unwrap()));
        record.pid = u32::MAX; // definitely not alive
        write_server_record(&path, &record).unwrap();

        assert!(active_server_record(dir.path()).is_none());
        assert!(!path.exists()); // lazy cleanup removed file
    }
}
