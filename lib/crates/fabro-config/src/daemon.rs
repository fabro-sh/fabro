#![expect(
    clippy::disallowed_methods,
    reason = "Server daemon metadata uses synchronous local file I/O and process probes."
)]

use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fabro_config::RuntimeDirectory;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::bind::Bind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerDaemon {
    pub pid:        u32,
    pub bind:       Bind,
    pub log_path:   PathBuf,
    pub started_at: DateTime<Utc>,
}

impl ServerDaemon {
    #[must_use]
    pub fn new(pid: u32, bind: Bind, log_path: PathBuf) -> Self {
        Self {
            pid,
            bind,
            log_path,
            started_at: Utc::now(),
        }
    }

    pub fn read(dir: &RuntimeDirectory) -> Result<Option<Self>> {
        let record_path = dir.record_path();
        let content = match std::fs::read_to_string(&record_path) {
            Ok(content) => content,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(anyhow::Error::new(err)
                    .context(format!("reading server record {}", record_path.display())));
            }
        };

        serde_json::from_str(&content)
            .map(Some)
            .with_context(|| format!("parsing server record {}", record_path.display()))
    }

    pub fn load_running(dir: &RuntimeDirectory) -> Result<Option<Self>> {
        let Some(daemon) = Self::read(dir)? else {
            return Ok(None);
        };

        if daemon.is_running() {
            Ok(Some(daemon))
        } else {
            Self::remove(dir);
            Ok(None)
        }
    }

    pub fn write(&self, dir: &RuntimeDirectory) -> Result<()> {
        let record_path = dir.record_path();
        let record_dir = record_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(record_dir).with_context(|| {
            format!("creating server record directory {}", record_dir.display())
        })?;

        let temp = NamedTempFile::new_in(record_dir).with_context(|| {
            format!("creating temp server record for {}", record_path.display())
        })?;
        std::fs::write(temp.path(), serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing temp server record for {}", record_path.display()))?;
        temp.persist(&record_path)
            .map_err(|err| err.error)
            .with_context(|| format!("persisting server record {}", record_path.display()))?;
        Ok(())
    }

    pub fn remove(dir: &RuntimeDirectory) {
        let record_path = dir.record_path();
        if let Err(err) = std::fs::remove_file(&record_path) {
            if err.kind() == ErrorKind::NotFound {
                return;
            }

            tracing::warn!(
                path = %record_path.display(),
                error = %err,
                "Failed to remove server record"
            );
        }
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        fabro_proc::process_running(self.pid) && server_process_matches(self.pid)
    }
}

#[cfg(unix)]
fn server_process_matches(pid: u32) -> bool {
    let output = match std::process::Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    let command = String::from_utf8_lossy(&output.stdout);
    command.contains("fabro") && command.contains("server")
}

#[cfg(not(unix))]
fn server_process_matches(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_config::RuntimeDirectory;

    use super::{Bind, ServerDaemon};

    fn test_daemon(bind: Bind) -> ServerDaemon {
        ServerDaemon {
            pid: std::process::id(),
            bind,
            log_path: PathBuf::from("/tmp/storage/logs/server.log"),
            started_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn write_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_directory = RuntimeDirectory::new(dir.path());
        let daemon = test_daemon(Bind::Tcp("127.0.0.1:3000".parse().unwrap()));
        daemon.write(&runtime_directory).unwrap();

        let loaded = ServerDaemon::read(&runtime_directory).unwrap().unwrap();
        assert_eq!(loaded, daemon);
    }

    #[test]
    fn active_server_record_returns_none_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_directory = RuntimeDirectory::new(dir.path());
        assert!(
            ServerDaemon::load_running(&runtime_directory)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn active_server_record_cleans_stale_dead_pid() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_directory = RuntimeDirectory::new(dir.path());
        let mut daemon = test_daemon(Bind::Tcp("127.0.0.1:3000".parse().unwrap()));
        daemon.pid = u32::MAX;
        daemon.write(&runtime_directory).unwrap();

        assert!(
            ServerDaemon::load_running(&runtime_directory)
                .unwrap()
                .is_none()
        );
        assert!(!runtime_directory.record_path().exists());
    }

    #[test]
    fn read_surfaces_parse_error_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_directory = RuntimeDirectory::new(dir.path());
        let record_path = runtime_directory.record_path();
        std::fs::create_dir_all(record_path.parent().unwrap()).unwrap();
        std::fs::write(&record_path, "not json").unwrap();

        let err = ServerDaemon::read(&runtime_directory).unwrap_err();
        assert!(
            err.to_string()
                .contains(record_path.display().to_string().as_str())
        );
    }
}
