use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::records::{ConclusionExt, RunRecord, RunRecordExt, StartRecord, StartRecordExt};
use crate::run_status::{RunStatus, RunStatusRecord, RunStatusRecordExt, StatusReason};

#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub run_id: String,
    pub dir_name: String,
    pub workflow_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug: Option<String>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<StatusReason>,
    pub start_time: String,
    pub labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_repo_path: Option<String>,
    pub goal: String,
    #[serde(skip)]
    pub start_time_dt: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub path: PathBuf,
    #[serde(skip)]
    pub is_orphan: bool,
}

pub fn default_storage_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".fabro")
}

pub fn logs_base(storage_dir: &Path) -> PathBuf {
    storage_dir.join("logs")
}

pub fn default_logs_base() -> PathBuf {
    logs_base(&default_storage_dir())
}

pub fn runs_base(storage_dir: &Path) -> PathBuf {
    storage_dir.join("runs")
}

pub fn default_runs_base() -> PathBuf {
    runs_base(&default_storage_dir())
}

pub fn scan_runs(base: &Path) -> Result<Vec<RunInfo>> {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };

    let mut runs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = entry.file_name().to_string_lossy().to_string();

        if let Ok(record) = RunRecord::load(&path) {
            let created_at = record.created_at;
            let start_time_dt = StartRecord::load(&path)
                .map(|s| s.start_time)
                .unwrap_or(created_at);
            let start_time = start_time_dt.to_rfc3339();
            let workflow_name = record.workflow_name().to_string();
            let goal = record.goal().to_string();
            let status_info = read_status(&path);

            runs.push(RunInfo {
                run_id: record.run_id,
                dir_name,
                workflow_name,
                workflow_slug: record.workflow_slug,
                status: status_info.status,
                status_reason: status_info.reason,
                start_time,
                labels: record.labels,
                duration_ms: status_info.duration_ms,
                total_cost: status_info.total_cost,
                host_repo_path: record.host_repo_path,
                start_time_dt: Some(created_at),
                end_time: status_info.end_time,
                path,
                goal,
                is_orphan: false,
            });
        } else {
            let mtime_dt = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|time| -> DateTime<Utc> { time.into() });
            let mtime = mtime_dt.map(|dt| dt.to_rfc3339()).unwrap_or_default();

            let run_id = std::fs::read_to_string(path.join("id.txt"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| dir_name.clone());

            let status_info = read_status(&path);
            let is_orphan = matches!(status_info.status, RunStatus::Dead);
            runs.push(RunInfo {
                run_id,
                dir_name,
                workflow_name: if is_orphan {
                    "[no run record]"
                } else {
                    "[starting]"
                }
                .to_string(),
                workflow_slug: None,
                status: status_info.status,
                status_reason: status_info.reason,
                start_time: mtime,
                labels: HashMap::new(),
                duration_ms: status_info.duration_ms,
                total_cost: status_info.total_cost,
                host_repo_path: None,
                start_time_dt: mtime_dt,
                end_time: status_info.end_time,
                path,
                goal: String::new(),
                is_orphan,
            });
        }
    }

    runs.sort_by(|a, b| b.start_time_dt.cmp(&a.start_time_dt));
    Ok(runs)
}

struct StatusInfo {
    status: RunStatus,
    reason: Option<StatusReason>,
    end_time: Option<DateTime<Utc>>,
    duration_ms: Option<u64>,
    total_cost: Option<f64>,
}

impl StatusInfo {
    fn simple(status: RunStatus) -> Self {
        Self {
            status,
            reason: None,
            end_time: None,
            duration_ms: None,
            total_cost: None,
        }
    }
}

fn read_status(run_dir: &Path) -> StatusInfo {
    if let Ok(record) = RunStatusRecord::load(&run_dir.join("status.json")) {
        if record.status.is_terminal() {
            if let Ok(conclusion) =
                crate::records::Conclusion::load(&run_dir.join("conclusion.json"))
            {
                return StatusInfo {
                    status: record.status,
                    reason: record.reason,
                    end_time: Some(conclusion.timestamp),
                    duration_ms: Some(conclusion.duration_ms),
                    total_cost: conclusion.total_cost,
                };
            }
        }
        return StatusInfo {
            status: record.status,
            reason: record.reason,
            end_time: None,
            duration_ms: None,
            total_cost: None,
        };
    }

    StatusInfo::simple(RunStatus::Dead)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    RunningOnly,
    All,
}

pub fn filter_runs(
    runs: &[RunInfo],
    before: Option<&str>,
    workflow: Option<&str>,
    labels: &[(String, String)],
    include_orphans: bool,
    status_filter: StatusFilter,
) -> Vec<RunInfo> {
    runs.iter()
        .filter(|run| {
            if status_filter == StatusFilter::RunningOnly && !run.status.is_active() {
                return false;
            }
            if run.is_orphan && !include_orphans {
                return false;
            }
            if let Some(before) = before {
                if !run.start_time.is_empty() && run.start_time.as_str() >= before {
                    return false;
                }
            }
            if let Some(pattern) = workflow {
                if !run.workflow_name.contains(pattern) {
                    return false;
                }
            }
            for (key, value) in labels {
                match run.labels.get(key) {
                    Some(current) if current == value => {}
                    _ => return false,
                }
            }
            true
        })
        .cloned()
        .collect()
}

pub fn find_run_by_prefix(base: &Path, prefix: &str) -> Result<PathBuf> {
    let runs = scan_runs(base).context("Failed to scan runs")?;
    let matches: Vec<_> = runs
        .iter()
        .filter(|run| run.run_id.starts_with(prefix))
        .collect();

    match matches.len() {
        0 => bail!("No run found matching prefix '{prefix}'"),
        1 => Ok(matches[0].path.clone()),
        count => {
            let ids: Vec<&str> = matches.iter().map(|run| run.run_id.as_str()).collect();
            bail!(
                "Ambiguous prefix '{prefix}': {count} runs match: {}",
                ids.join(", ")
            )
        }
    }
}

pub fn resolve_run(base: &Path, identifier: &str) -> Result<RunInfo> {
    let runs = scan_runs(base).context("Failed to scan runs")?;

    let id_matches: Vec<_> = runs
        .iter()
        .filter(|run| run.run_id.starts_with(identifier))
        .collect();

    match id_matches.len() {
        1 => return Ok(id_matches[0].clone()),
        count if count > 1 => {
            let ids: Vec<&str> = id_matches.iter().map(|run| run.run_id.as_str()).collect();
            bail!(
                "Ambiguous prefix '{identifier}': {count} runs match: {}",
                ids.join(", ")
            )
        }
        _ => {}
    }

    let id_lower = identifier.to_lowercase();
    let id_collapsed = collapse_separators(&id_lower);
    let workflow_match = runs.iter().filter(|run| !run.is_orphan).find(|run| {
        if let Some(slug) = &run.workflow_slug {
            if slug.to_lowercase() == id_lower {
                return true;
            }
        }
        let name_lower = run.workflow_name.to_lowercase();
        name_lower.contains(&id_lower) || collapse_separators(&name_lower).contains(&id_collapsed)
    });

    match workflow_match {
        Some(run) => Ok(run.clone()),
        None => {
            bail!("No run found matching '{identifier}' (tried run ID prefix and workflow name)")
        }
    }
}

fn collapse_separators(s: &str) -> String {
    s.chars().filter(|c| *c != '-' && *c != '_').collect()
}
