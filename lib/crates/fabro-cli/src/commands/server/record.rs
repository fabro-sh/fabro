use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fabro_config::Storage;
use fabro_server::bind::Bind;
use fabro_util::Home;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ServerRecord {
    pub pid:            u32,
    pub bind:           Bind,
    pub log_path:       PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_token_path: Option<PathBuf>,
    pub started_at:     DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveServerRecord {
    pub record:      ServerRecord,
    pub record_path: PathBuf,
}

pub(crate) fn write_server_record(path: &Path, record: &ServerRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating server record directory {}", parent.display()))?;
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

fn server_record_path(storage_dir: &Path) -> PathBuf {
    Storage::new(storage_dir).server_state().record_path()
}

fn legacy_record_path(storage_dir: &Path) -> Option<PathBuf> {
    if storage_dir == fabro_config::user::default_storage_dir() {
        Some(Home::from_env().root().join("server.json"))
    } else {
        None
    }
}

fn active_server_record_at_path(path: PathBuf) -> Option<ActiveServerRecord> {
    let record = read_server_record(&path)?;
    if server_record_is_running(&record) {
        Some(ActiveServerRecord {
            record,
            record_path: path,
        })
    } else {
        remove_server_record(&path);
        None
    }
}

pub(crate) fn active_server_record_details(
    storage_dir: &Path,
) -> Result<Option<ActiveServerRecord>> {
    let primary_path = server_record_path(storage_dir);
    if let Some(active) = active_server_record_at_path(primary_path.clone()) {
        return Ok(Some(active));
    }

    if let Some(legacy_path) = legacy_record_path(storage_dir) {
        if active_server_record_at_path(legacy_path.clone()).is_some() {
            bail!(
                "Legacy server record {} is still active while current storage record {} is missing.\nStop the old daemon with a legacy Fabro CLI or manually clear the stale daemon before retrying.",
                legacy_path.display(),
                primary_path.display()
            );
        }
    }

    Ok(None)
}

pub(crate) fn active_server_record(storage_dir: &Path) -> Result<Option<ServerRecord>> {
    Ok(active_server_record_details(storage_dir)?.map(|active| active.record))
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "This synchronous process identity probe is shared by async server start and sync server status flows."
)]
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
            log_path: PathBuf::from("/tmp/storage/logs/server.log"),
            dev_token_path: None,
            started_at: Utc::now(),
        }
    }

    #[test]
    fn write_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = Storage::new(dir.path()).server_state().record_path();
        let record = test_record(Bind::Tcp("127.0.0.1:3000".parse().unwrap()));
        write_server_record(&path, &record).unwrap();

        let loaded = read_server_record(&path).unwrap();
        assert_eq!(loaded.pid, record.pid);
        assert_eq!(loaded.bind, record.bind);
    }

    #[test]
    fn active_server_record_returns_none_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(active_server_record(dir.path()).unwrap().is_none());
    }

    #[test]
    fn active_server_record_cleans_stale_dead_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = Storage::new(dir.path()).server_state().record_path();
        let mut record = test_record(Bind::Tcp("127.0.0.1:3000".parse().unwrap()));
        record.pid = u32::MAX; // definitely not alive
        write_server_record(&path, &record).unwrap();

        assert!(active_server_record(dir.path()).unwrap().is_none());
        assert!(!path.exists()); // lazy cleanup removed file
    }
}
