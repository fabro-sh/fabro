use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fabro_config::Storage;
use fabro_store::{Database, RunSummary};
use fabro_types::RunId;
use serde::Serialize;

use crate::operations::make_run_dir;
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
            .map(|summary| summary.run_id)
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
            .and_then(|summary| summary.start_time.or(Some(summary.run_id.created_at())))
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
        self.summary
            .as_ref()
            .and_then(|summary| summary.total_usd_micros)
            .map(|value| value as f64 / 1_000_000.0)
    }

    pub fn total_usd_micros(&self) -> Option<i64> {
        self.summary
            .as_ref()
            .and_then(|summary| summary.total_usd_micros)
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

pub fn scratch_base(storage_dir: &Path) -> PathBuf {
    Storage::new(storage_dir).scratch_dir()
}

pub fn default_scratch_base() -> PathBuf {
    scratch_base(&fabro_util::Home::from_env().storage_dir())
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
        if parse_run_id(&dir_name).is_none() {
            continue;
        }

        let mtime_dt = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|time| -> DateTime<Utc> { time.into() });

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

    runs.sort_by(|a, b| {
        b.start_time_dt
            .cmp(&a.start_time_dt)
            .then_with(|| b.run_id().cmp(&a.run_id()))
    });
    Ok(runs)
}

pub async fn scan_runs_combined(store: &Database, base: &Path) -> Result<Vec<RunInfo>> {
    let store_runs = store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
        .unwrap_or_default();
    scan_runs_with_summaries(&store_runs, base)
}

pub fn scan_runs_with_summaries(summaries: &[RunSummary], base: &Path) -> Result<Vec<RunInfo>> {
    let mut runs_by_id: HashMap<RunId, RunInfo> = HashMap::new();

    for summary in summaries {
        let Some(run_info) = run_info_from_summary(summary, base) else {
            continue;
        };
        runs_by_id.insert(run_info.run_id(), run_info);
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
    runs.sort_by(|a, b| {
        b.start_time_dt
            .cmp(&a.start_time_dt)
            .then_with(|| b.run_id().cmp(&a.run_id()))
    });
    Ok(runs)
}

fn run_info_from_summary(summary: &RunSummary, scratch_base: &Path) -> Option<RunInfo> {
    let path = make_run_dir(scratch_base, &summary.run_id);
    if !path.exists() {
        return None;
    }
    let dir_name = path.file_name()?.to_string_lossy().to_string();
    let start_time_dt = summary.run_id.created_at();
    let end_time = if summary.status.is_some_and(RunStatus::is_terminal) {
        summary.duration_ms.and_then(|duration_ms| {
            Some(start_time_dt + chrono::Duration::milliseconds(i64::try_from(duration_ms).ok()?))
        })
    } else {
        None
    };

    Some(RunInfo::new(
        Some(summary.clone()),
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
    store: &Database,
    base: &Path,
    identifier: &str,
) -> Result<RunInfo> {
    let runs = scan_runs_combined(store, base)
        .await
        .context("Failed to scan runs")?;
    resolve_run_from_infos(&runs, identifier)
}

pub fn resolve_run_from_summaries(
    summaries: &[RunSummary],
    base: &Path,
    identifier: &str,
) -> Result<RunInfo> {
    let runs = scan_runs_with_summaries(summaries, base).context("Failed to scan runs")?;
    resolve_run_from_infos(&runs, identifier)
}

fn resolve_run_from_infos(runs: &[RunInfo], identifier: &str) -> Result<RunInfo> {
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
    let workflow_match = runs
        .iter()
        .filter(|run| !run.is_orphan)
        .filter(|run| {
            if let Some(slug) = run.workflow_slug() {
                if slug.to_lowercase() == id_lower {
                    return true;
                }
            }
            let name_lower = run.workflow_name().to_lowercase();
            name_lower.contains(&id_lower)
                || collapse_separators(&name_lower).contains(&id_collapsed)
        })
        .max_by_key(|run| run.run_id().created_at());

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
    if value.is_empty() {
        return None;
    }
    // Try direct ULID parse first, then try extracting ULID after date prefix
    // (YYYYMMDD-ULID).
    value.parse().ok().or_else(|| {
        value
            .split_once('-')
            .and_then(|(_, ulid)| ulid.parse().ok())
    })
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

    use fabro_graphviz::graph::Graph;
    use fabro_store::Database;
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{RunStatus, fixtures};
    use object_store::memory::InMemory;

    use super::scan_runs_combined;
    use crate::event::{Event, append_event};
    use crate::operations::make_run_dir;
    use crate::records::RunRecord;

    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    fn sample_run_record() -> RunRecord {
        RunRecord {
            run_id: fixtures::RUN_1,
            settings: SettingsLayer::default(),
            graph: Graph::new("test"),
            workflow_slug: Some("test".to_string()),
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path: Some("/tmp/project".to_string()),
            repo_origin_url: None,
            base_branch: Some("main".to_string()),
            labels: HashMap::new(),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
        }
    }

    #[tokio::test]
    async fn scan_runs_combined_uses_store_status_without_status_json() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = make_run_dir(temp.path(), &fixtures::RUN_1);
        std::fs::create_dir_all(&run_dir).unwrap();

        let store = memory_store();
        let run_record = sample_run_record();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        append_event(
            &run_store,
            &fixtures::RUN_1,
            &Event::RunCreated {
                run_id: fixtures::RUN_1,
                settings: serde_json::to_value(&run_record.settings).unwrap(),
                graph: serde_json::to_value(&run_record.graph).unwrap(),
                workflow_source: None,
                workflow_config: None,
                labels: run_record.labels.clone().into_iter().collect(),
                run_dir: run_dir.display().to_string(),
                working_directory: run_record.working_directory.display().to_string(),
                host_repo_path: run_record.host_repo_path.clone(),
                repo_origin_url: run_record.repo_origin_url.clone(),
                base_branch: run_record.base_branch.clone(),
                workflow_slug: run_record.workflow_slug.clone(),
                db_prefix: None,
                provenance: run_record.provenance.clone(),
                manifest_blob: None,
            },
        )
        .await
        .unwrap();
        append_event(
            &run_store,
            &fixtures::RUN_1,
            &Event::RunSubmitted {
                reason: None,
                definition_blob: None,
            },
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
