use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use fabro_types::{RunId, RunStatus, RunSummary, StatusReason};

use crate::server_client::Client;

#[derive(Debug, Clone)]
pub(crate) struct ServerRunSummaryInfo {
    summary: RunSummary,
}

impl ServerRunSummaryInfo {
    pub(crate) fn from_summary(summary: RunSummary) -> Self {
        Self { summary }
    }

    pub(crate) fn run_id(&self) -> RunId {
        self.summary.run_id
    }

    pub(crate) fn workflow_name(&self) -> String {
        self.summary
            .workflow_name
            .clone()
            .unwrap_or_else(|| "[no run spec]".to_string())
    }

    pub(crate) fn workflow_slug(&self) -> Option<&str> {
        self.summary.workflow_slug.as_deref()
    }

    pub(crate) fn status(&self) -> RunStatus {
        self.summary.status
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
    client: Arc<Client>,
    runs:   Vec<ServerRunSummaryInfo>,
}

impl ServerSummaryLookup {
    pub(crate) async fn from_client(client: Arc<Client>) -> Result<Self> {
        let summaries = client.list_store_runs().await?;
        let mut runs = summaries
            .into_iter()
            .map(ServerRunSummaryInfo::from_summary)
            .collect::<Vec<_>>();
        runs.sort_by(|a, b| {
            b.start_time_dt()
                .cmp(&a.start_time_dt())
                .then_with(|| b.run_id().cmp(&a.run_id()))
        });
        Ok(Self { client, runs })
    }

    pub(crate) fn client(&self) -> &Client {
        self.client.as_ref()
    }

    pub(crate) fn runs(&self) -> &[ServerRunSummaryInfo] {
        &self.runs
    }
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
