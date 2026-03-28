use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fabro_config::FabroSettingsExt;
use fabro_workflows::records::{RunRecord, RunRecordExt};
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

pub(crate) fn active_launcher_record_for_run(run_dir: &Path) -> Option<LauncherRecord> {
    let run_record = RunRecord::load(run_dir).ok()?;
    let path = launcher_record_path(&run_record.settings.storage_dir(), &run_record.run_id);
    let launcher = read_launcher_record(&path)?;
    if launcher_record_is_running(&launcher) {
        Some(launcher)
    } else {
        remove_launcher_record(&path);
        None
    }
}

pub(crate) fn launcher_record_is_running(record: &LauncherRecord) -> bool {
    process_alive(record.pid) && launcher_process_matches(record)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn launcher_process_matches(record: &LauncherRecord) -> bool {
    let output = match std::process::Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &record.pid.to_string()])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };

    let command = String::from_utf8_lossy(&output.stdout);
    command_matches_launcher(record, &command)
}

#[cfg(unix)]
fn command_matches_launcher(record: &LauncherRecord, command: &str) -> bool {
    let run_dir = record.run_dir.to_string_lossy();
    let old_match = command.contains("__detached") && command.contains(run_dir.as_ref());
    let new_match = command.contains(&format!("fabro: {}", super::short_run_id(&record.run_id)));
    old_match || new_match
}

#[cfg(not(unix))]
fn launcher_process_matches(_record: &LauncherRecord) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use fabro_config::FabroSettings;
    use fabro_graphviz::graph::Graph;
    use fabro_workflows::records::RunRecord;

    #[test]
    fn active_launcher_record_for_run_removes_stale_record() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        RunRecord {
            run_id: "run-test".to_string(),
            created_at: Utc::now(),
            settings: FabroSettings {
                storage_dir: Some(storage_dir.clone()),
                ..Default::default()
            },
            graph: Graph::default(),
            workflow_slug: None,
            working_directory: dir.path().to_path_buf(),
            host_repo_path: None,
            base_branch: None,
            labels: std::collections::HashMap::new(),
        }
        .save(&run_dir)
        .unwrap();

        let launcher_path = launcher_record_path(&storage_dir, "run-test");
        write_launcher_record(
            &launcher_path,
            &LauncherRecord {
                run_id: "run-test".to_string(),
                run_dir: run_dir.clone(),
                pid: u32::MAX,
                resume: false,
                log_path: dir.path().join("launcher.log"),
                started_at: Utc::now(),
            },
        )
        .unwrap();

        assert!(active_launcher_record_for_run(&run_dir).is_none());
        assert!(!launcher_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn command_matches_launcher_accepts_old_detached_format() {
        let dir = tempfile::tempdir().unwrap();
        let record = LauncherRecord {
            run_id: "01JGABCDEF12ZYXW".to_string(),
            run_dir: dir.path().join("run"),
            pid: 42,
            resume: false,
            log_path: dir.path().join("launcher.log"),
            started_at: Utc::now(),
        };

        let command = format!(
            "/usr/local/bin/fabro __detached --run-dir {} --launcher-path /tmp/launcher.json",
            record.run_dir.display()
        );

        assert!(command_matches_launcher(&record, &command));
    }

    #[cfg(unix)]
    #[test]
    fn command_matches_launcher_accepts_new_title_format() {
        let dir = tempfile::tempdir().unwrap();
        let record = LauncherRecord {
            run_id: "01JGABCDEF12ZYXW".to_string(),
            run_dir: dir.path().join("run"),
            pid: 42,
            resume: false,
            log_path: dir.path().join("launcher.log"),
            started_at: Utc::now(),
        };

        assert!(command_matches_launcher(
            &record,
            "fabro: 01JGABCDEF12 plan"
        ));
    }
}
