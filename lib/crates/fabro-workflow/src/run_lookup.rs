use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fabro_store::{RunSummary, SlateStore};
use fabro_types::RunId;
use serde::Serialize;

use crate::run_status::{RunStatus, StatusReason};

#[derive(Debug, Clone)]
struct RunLocalState {
    dir_name: String,
    start_time_dt: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
    path: PathBuf,
    is_orphan: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    #[serde(skip)]
    summary: Option<RunSummary>,
    pub dir_name: String,
    #[serde(skip)]
    pub start_time_dt: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub path: PathBuf,
    #[serde(skip)]
    pub is_orphan: bool,
}

impl RunInfo {
    fn new(summary: Option<RunSummary>, local: RunLocalState) -> Self {
        Self {
            summary,
            dir_name: local.dir_name,
            start_time_dt: local.start_time_dt,
            end_time: local.end_time,
            path: local.path,
            is_orphan: local.is_orphan,
        }
    }

    pub fn run_id(&self) -> RunId {
        self.summary
            .as_ref()
            .map(|summary| summary.catalog.run_id)
            .or_else(|| parse_run_id(&self.dir_name))
            .expect("RunInfo must have a run id")
    }

    pub fn workflow_name(&self) -> String {
        self.summary
            .as_ref()
            .and_then(|summary| summary.workflow_name.clone())
            .unwrap_or_else(|| "[no run record]".to_string())
    }

    pub fn workflow_slug(&self) -> Option<&str> {
        self.summary
            .as_ref()
            .and_then(|summary| summary.workflow_slug.as_deref())
    }

    pub fn status(&self) -> RunStatus {
        self.summary
            .as_ref()
            .and_then(|summary| summary.status)
            .unwrap_or(RunStatus::Dead)
    }

    pub fn status_reason(&self) -> Option<StatusReason> {
        self.summary
            .as_ref()
            .and_then(|summary| summary.status_reason)
    }

    pub fn start_time(&self) -> String {
        self.summary
            .as_ref()
            .and_then(|summary| summary.start_time.or(Some(summary.catalog.created_at)))
            .or(self.start_time_dt)
            .map(|time| time.to_rfc3339())
            .unwrap_or_default()
    }

    pub fn labels(&self) -> &HashMap<String, String> {
        if let Some(summary) = self.summary.as_ref() {
            &summary.labels
        } else {
            empty_labels()
        }
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.summary
            .as_ref()
            .and_then(|summary| summary.duration_ms)
    }

    pub fn total_cost(&self) -> Option<f64> {
        self.summary.as_ref().and_then(|summary| summary.total_cost)
    }

    pub fn host_repo_path(&self) -> Option<&str> {
        self.summary
            .as_ref()
            .and_then(|summary| summary.host_repo_path.as_deref())
    }

    pub fn goal(&self) -> String {
        self.summary
            .as_ref()
            .and_then(|summary| summary.goal.clone())
            .unwrap_or_default()
    }
}

fn empty_labels() -> &'static HashMap<String, String> {
    static EMPTY: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

pub fn default_storage_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".fabro")
}

pub fn logs_base(storage_dir: &Path) -> PathBuf {
    storage_dir.join("logs")
}

pub fn runs_base(storage_dir: &Path) -> PathBuf {
    storage_dir.join("runs")
}

pub fn default_runs_base() -> PathBuf {
    runs_base(&default_storage_dir())
}

fn scan_orphan_runs(base: &Path) -> Result<Vec<RunInfo>> {
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
        let mtime_dt = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|time| -> DateTime<Utc> { time.into() });

        let run_id = std::fs::read_to_string(path.join("id.txt"))
            .ok()
            .and_then(|s| parse_run_id(&s))
            .or_else(|| parse_run_id(&dir_name));
        if run_id.is_none() {
            continue;
        }

        runs.push(RunInfo::new(
            None,
            RunLocalState {
                dir_name,
                start_time_dt: mtime_dt,
                end_time: None,
                path,
                is_orphan: true,
            },
        ));
    }

    runs.sort_by(|a, b| b.start_time_dt.cmp(&a.start_time_dt));
    Ok(runs)
}

pub async fn scan_runs_combined(store: &SlateStore, base: &Path) -> Result<Vec<RunInfo>> {
    let mut runs_by_id: HashMap<RunId, RunInfo> = HashMap::new();

    if let Ok(store_runs) = store.list_runs().await {
        for summary in store_runs {
            let Some(run_info) = run_info_from_summary(summary) else {
                continue;
            };
            runs_by_id.insert(run_info.run_id(), run_info);
        }
    }

    let store_run_ids = runs_by_id
        .keys()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for run in scan_orphan_runs(base)?
        .into_iter()
        .filter(|run| run.is_orphan && !store_run_ids.contains(&run.run_id()))
    {
        runs_by_id.insert(run.run_id(), run);
    }

    let mut runs: Vec<_> = runs_by_id.into_values().collect();
    runs.sort_by(|a, b| b.start_time_dt.cmp(&a.start_time_dt));
    Ok(runs)
}

fn run_info_from_summary(summary: RunSummary) -> Option<RunInfo> {
    let run_dir = summary.catalog.run_dir.as_deref()?;
    let path = PathBuf::from(run_dir);
    if !path.exists() {
        return None;
    }
    let dir_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())?;
    let start_time_dt = summary.catalog.created_at;
    let end_time = if summary.status.is_some_and(RunStatus::is_terminal) {
        summary.duration_ms.and_then(|duration_ms| {
            Some(start_time_dt + chrono::Duration::milliseconds(i64::try_from(duration_ms).ok()?))
        })
    } else {
        None
    };

    Some(RunInfo::new(
        Some(summary),
        RunLocalState {
            dir_name,
            start_time_dt: Some(start_time_dt),
            end_time,
            path,
            is_orphan: false,
        },
    ))
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
            if status_filter == StatusFilter::RunningOnly && !run.status().is_active() {
                return false;
            }
            if run.is_orphan && !include_orphans {
                return false;
            }
            if let Some(before) = before {
                let start_time = run.start_time();
                if !start_time.is_empty() && start_time.as_str() >= before {
                    return false;
                }
            }
            if let Some(pattern) = workflow {
                if !run.workflow_name().contains(pattern) {
                    return false;
                }
            }
            for (key, value) in labels {
                match run.labels().get(key) {
                    Some(current) if current == value => {}
                    _ => return false,
                }
            }
            true
        })
        .cloned()
        .collect()
}

pub async fn resolve_run_combined(
    store: &SlateStore,
    base: &Path,
    identifier: &str,
) -> Result<RunInfo> {
    let runs = scan_runs_combined(store, base)
        .await
        .context("Failed to scan runs")?;

    let id_matches: Vec<_> = runs
        .iter()
        .filter(|run| run_id_matches(run.run_id(), identifier))
        .collect();

    match id_matches.len() {
        1 => return Ok(id_matches[0].clone()),
        count if count > 1 => {
            let ids: Vec<String> = id_matches
                .iter()
                .map(|run| run.run_id().to_string())
                .collect();
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
        if let Some(slug) = run.workflow_slug() {
            if slug.to_lowercase() == id_lower {
                return true;
            }
        }
        let name_lower = run.workflow_name().to_lowercase();
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

fn parse_run_id(value: &str) -> Option<RunId> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)?.parse().ok()
}

fn run_id_matches(run_id: RunId, prefix: &str) -> bool {
    run_id.to_string().starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Utc;
    use fabro_graphviz::graph::Graph;
    use fabro_store::{SlateStore, StoreHandle};
    use fabro_types::{RunStatus, Settings, fixtures};
    use object_store::memory::InMemory;

    use super::scan_runs_combined;
    use crate::event::{WorkflowRunEvent, append_workflow_event};
    use crate::records::RunRecord;

    fn memory_store() -> StoreHandle {
        Arc::new(SlateStore::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    fn sample_run_record() -> RunRecord {
        RunRecord {
            run_id: fixtures::RUN_1,
            created_at: Utc::now(),
            settings: Settings::default(),
            graph: Graph::new("test"),
            workflow_slug: Some("test".to_string()),
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path: Some("/tmp/project".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn scan_runs_combined_uses_store_status_without_status_json() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join(fixtures::RUN_1.to_string());
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("id.txt"), format!("{}\n", fixtures::RUN_1)).unwrap();

        let store = memory_store();
        let run_dir_string = run_dir.to_string_lossy().to_string();
        let run_record = sample_run_record();
        let run_store = store
            .create_run(
                &fixtures::RUN_1,
                run_record.created_at,
                Some(&run_dir_string),
            )
            .await
            .unwrap();
        append_workflow_event(
            &run_store,
            &fixtures::RUN_1,
            &WorkflowRunEvent::RunCreated {
                run_id: fixtures::RUN_1,
                settings: serde_json::to_value(&run_record.settings).unwrap(),
                graph: serde_json::to_value(&run_record.graph).unwrap(),
                workflow_source: None,
                workflow_config: None,
                labels: run_record.labels.clone().into_iter().collect(),
                run_dir: run_dir_string.clone(),
                working_directory: run_record.working_directory.display().to_string(),
                host_repo_path: run_record.host_repo_path.clone(),
                base_branch: run_record.base_branch.clone(),
                workflow_slug: run_record.workflow_slug.clone(),
                db_prefix: None,
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            &run_store,
            &fixtures::RUN_1,
            &WorkflowRunEvent::RunSubmitted { reason: None },
        )
        .await
        .unwrap();

        let runs = scan_runs_combined(&store, temp.path()).await.unwrap();
        let run = runs
            .iter()
            .find(|run| run.run_id() == fixtures::RUN_1)
            .expect("run should be listed");

        assert_eq!(run.status(), RunStatus::Submitted);
        assert!(!run.is_orphan);
    }
}
