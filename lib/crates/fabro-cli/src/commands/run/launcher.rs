use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fabro_types::RunId;
use fabro_workflow::records::{RunRecord, RunRecordExt};
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::commands::run::short_run_id;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LauncherRecord {
    pub run_id: RunId,
    pub run_dir: PathBuf,
    pub pid: u32,
    pub resume: bool,
    pub log_path: PathBuf,
    pub started_at: DateTime<Utc>,
}

pub(crate) fn launcher_dir(storage_dir: &Path) -> PathBuf {
    storage_dir.join("launchers")
}

pub(crate) fn launcher_record_path(storage_dir: &Path, run_id: &RunId) -> PathBuf {
    launcher_dir(storage_dir).join(format!("{run_id}.json"))
}

pub(crate) fn launcher_log_path(storage_dir: &Path, run_id: &RunId) -> PathBuf {
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
    let launcher = launcher_record_for_run(run_dir)?;
    let storage_dir = run_dir.parent()?.parent()?;
    let path = launcher_record_path(storage_dir, &launcher.run_id);
    if launcher_record_is_running(&launcher) {
        Some(launcher)
    } else {
        remove_launcher_record(&path);
        None
    }
}

pub(crate) fn launcher_record_for_run(run_dir: &Path) -> Option<LauncherRecord> {
    if let Ok(run_record) = RunRecord::load(run_dir) {
        return read_launcher_record(&launcher_record_path(
            &run_record.settings.storage_dir(),
            &run_record.run_id,
        ));
    }

    let storage_dir = run_dir.parent()?.parent()?;
    let launchers_dir = launcher_dir(storage_dir);
    let entries = std::fs::read_dir(&launchers_dir).ok()?;
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let Some(launcher) = read_launcher_record(&path) else {
            continue;
        };
        if launcher.run_dir == run_dir {
            return Some(launcher);
        }
    }

    None
}

pub(crate) fn active_launcher_record(storage_dir: &Path, run_id: &RunId) -> Option<LauncherRecord> {
    let path = launcher_record_path(storage_dir, run_id);
    let launcher = read_launcher_record(&path)?;
    if launcher_record_is_running(&launcher) {
        Some(launcher)
    } else {
        remove_launcher_record(&path);
        None
    }
}

pub(crate) fn launcher_record_is_running(record: &LauncherRecord) -> bool {
    fabro_proc::process_alive(record.pid) && launcher_process_matches(record)
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
    let run_id = record.run_id.to_string();
    let new_match = command.contains(&format!("fabro: {}", super::short_run_id(&run_id)));
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
    use fabro_graphviz::graph::Graph;
    use fabro_types::{Settings, fixtures};
    use fabro_workflow::records::RunRecord;

    #[test]
    fn active_launcher_record_for_run_removes_stale_record() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        RunRecord {
            run_id: fixtures::RUN_1,
            created_at: Utc::now(),
            settings: Settings {
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

        let launcher_path = launcher_record_path(&storage_dir, &fixtures::RUN_1);
        write_launcher_record(
            &launcher_path,
            &LauncherRecord {
                run_id: fixtures::RUN_1,
                run_dir: run_dir.clone(),
                pid: u32::MAX,
                resume: false,
                log_path: dir.path().join("launcher.log"),
                started_at: Utc::now(),
            },
        )
        .unwrap();

        assert!(active_launcher_record_for_run(&run_dir).is_none());
        assert!(active_launcher_record(&storage_dir, &fixtures::RUN_1).is_none());
        assert!(!launcher_path.exists());
    }

    #[test]
    fn active_launcher_record_for_run_falls_back_without_run_json() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        let run_dir = storage_dir.join("runs").join("20260401-test");
        std::fs::create_dir_all(&run_dir).unwrap();

        let launcher_path = launcher_record_path(&storage_dir, &fixtures::RUN_1);
        write_launcher_record(
            &launcher_path,
            &LauncherRecord {
                run_id: fixtures::RUN_1,
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
            run_id: fixtures::RUN_2,
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
            run_id: fixtures::RUN_3,
            run_dir: dir.path().join("run"),
            pid: 42,
            resume: false,
            log_path: dir.path().join("launcher.log"),
            started_at: Utc::now(),
        };

        assert!(command_matches_launcher(
            &record,
            &format!("fabro: {} plan", short_run_id(&record.run_id.to_string()))
        ));
    }
}
