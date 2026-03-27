use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LauncherRecord {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub pid: u32,
    pub resume: bool,
    pub log_path: PathBuf,
    pub started_at: DateTime<Utc>,
}

pub(crate) fn launcher_dir(storage_dir: &Path) -> PathBuf {
    storage_dir.join("launchers")
}

pub(crate) fn launcher_record_path(storage_dir: &Path, run_id: &str) -> PathBuf {
    launcher_dir(storage_dir).join(format!("{run_id}.json"))
}

pub(crate) fn launcher_log_path(storage_dir: &Path, run_id: &str) -> PathBuf {
    launcher_dir(storage_dir).join(format!("{run_id}.log"))
}

pub(crate) fn write_launcher_record(path: &Path, record: &LauncherRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(record)?)
        .with_context(|| format!("Failed to write launcher metadata to {}", path.display()))
}

pub(crate) fn read_launcher_record(path: &Path) -> Option<LauncherRecord> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(crate) fn remove_launcher_record(path: &Path) {
    let _ = std::fs::remove_file(path);
}

pub(crate) fn launcher_record_for_run(run_dir: &Path) -> Option<LauncherRecord> {
    let record = fabro_workflows::records::RunRecord::load(run_dir).ok()?;
    let path = launcher_record_path(&record.settings.storage_dir(), &record.run_id);
    read_launcher_record(&path)
}
