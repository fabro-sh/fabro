use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use fabro_store::RunSummary;
use fabro_types::{RunId, RunStatus, StatusReason};
use fabro_workflow::run_lookup::{RunInfo, resolve_run_from_summaries, scratch_base};

use crate::server_client::{self, ServerStoreClient};

pub(crate) struct ServerRunLookup {
    client: ServerStoreClient,
    scratch_base: PathBuf,
    summaries: Vec<RunSummary>,
}

impl ServerRunLookup {
    pub(crate) async fn connect(storage_dir: &Path) -> Result<Self> {
        Self::connect_from_scratch_base(&scratch_base(storage_dir)).await
    }

    pub(crate) async fn connect_from_scratch_base(scratch_base: &Path) -> Result<Self> {
        let storage_dir = scratch_base.parent().unwrap_or(scratch_base);
        let client = server_client::connect_server(storage_dir).await?;
        let summaries = client.list_store_runs().await?;
        Ok(Self {
            client,
            scratch_base: scratch_base.to_path_buf(),
            summaries,
        })
    }

    pub(crate) fn client(&self) -> &ServerStoreClient {
        &self.client
    }

    pub(crate) fn resolve(&self, selector: &str) -> Result<RunInfo> {
        resolve_run_from_summaries(&self.summaries, &self.scratch_base, selector)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ServerRunSummaryInfo {
    summary: RunSummary,
}

impl ServerRunSummaryInfo {
    pub(crate) fn run_id(&self) -> RunId {
        self.summary.run_id
    }

    pub(crate) fn workflow_name(&self) -> String {
        self.summary
            .workflow_name
            .clone()
            .unwrap_or_else(|| "[no run record]".to_string())
    }

    pub(crate) fn workflow_slug(&self) -> Option<&str> {
        self.summary.workflow_slug.as_deref()
    }

    pub(crate) fn status(&self) -> RunStatus {
        self.summary.status.unwrap_or(RunStatus::Dead)
    }

    pub(crate) fn status_reason(&self) -> Option<StatusReason> {
        self.summary.status_reason
    }

    pub(crate) fn start_time(&self) -> String {
        self.start_time_dt()
            .map(|time| time.to_rfc3339())
            .unwrap_or_default()
    }

    pub(crate) fn start_time_dt(&self) -> Option<DateTime<Utc>> {
        self.summary
            .start_time
            .or(Some(self.summary.run_id.created_at()))
    }

    pub(crate) fn labels(&self) -> &HashMap<String, String> {
        &self.summary.labels
    }

    pub(crate) fn duration_ms(&self) -> Option<u64> {
        self.summary.duration_ms
    }

    pub(crate) fn total_usd_micros(&self) -> Option<i64> {
        self.summary.total_usd_micros
    }

    pub(crate) fn host_repo_path(&self) -> Option<&str> {
        self.summary.host_repo_path.as_deref()
    }

    pub(crate) fn goal(&self) -> String {
        self.summary.goal.clone().unwrap_or_default()
    }
}

pub(crate) struct ServerSummaryLookup {
    client: Arc<ServerStoreClient>,
    runs: Vec<ServerRunSummaryInfo>,
}

impl ServerSummaryLookup {
    pub(crate) async fn from_client(client: Arc<ServerStoreClient>) -> Result<Self> {
        let summaries = client.list_store_runs().await?;
        let mut runs = summaries
            .into_iter()
            .map(|summary| ServerRunSummaryInfo { summary })
            .collect::<Vec<_>>();
        runs.sort_by(|a, b| {
            b.start_time_dt()
                .cmp(&a.start_time_dt())
                .then_with(|| b.run_id().cmp(&a.run_id()))
        });
        Ok(Self { client, runs })
    }

    pub(crate) fn client(&self) -> &ServerStoreClient {
        self.client.as_ref()
    }

    pub(crate) fn runs(&self) -> &[ServerRunSummaryInfo] {
        &self.runs
    }

    pub(crate) fn resolve(&self, selector: &str) -> Result<ServerRunSummaryInfo> {
        resolve_server_run_from_infos(&self.runs, selector)
    }
}

pub(crate) fn resolve_server_run_from_summaries(
    runs: &[ServerRunSummaryInfo],
    selector: &str,
) -> Result<ServerRunSummaryInfo> {
    resolve_server_run_from_infos(runs, selector)
}

pub(crate) fn filter_server_runs(
    runs: &[ServerRunSummaryInfo],
    before: Option<&str>,
    workflow: Option<&str>,
    labels: &[(String, String)],
    running_only: bool,
) -> Vec<ServerRunSummaryInfo> {
    runs.iter()
        .filter(|run| !running_only || run.status().is_active())
        .filter(|run| {
            before.is_none_or(|before| {
                let start_time = run.start_time();
                start_time.is_empty() || start_time.as_str() < before
            })
        })
        .filter(|run| workflow.is_none_or(|pattern| run.workflow_name().contains(pattern)))
        .filter(|run| {
            labels.iter().all(|(key, value)| {
                run.labels()
                    .get(key)
                    .is_some_and(|current| current == value)
            })
        })
        .cloned()
        .collect()
}

fn resolve_server_run_from_infos(
    runs: &[ServerRunSummaryInfo],
    identifier: &str,
) -> Result<ServerRunSummaryInfo> {
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
            );
        }
        _ => {}
    }

    let id_lower = identifier.to_lowercase();
    let id_collapsed = collapse_separators(&id_lower);
    let workflow_match = runs
        .iter()
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

fn run_id_matches(run_id: RunId, prefix: &str) -> bool {
    run_id.to_string().starts_with(prefix)
}
