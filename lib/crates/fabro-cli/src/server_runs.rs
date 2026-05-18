use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use fabro_types::{Run, RunId, RunStatus};

use crate::server_client::Client;

#[derive(Debug, Clone)]
pub(crate) struct ServerRunInfo {
    run: Run,
}

impl ServerRunInfo {
    pub(crate) fn from_run(run: Run) -> Self {
        Self { run }
    }

    pub(crate) fn run_id(&self) -> RunId {
        self.run.id
    }

    pub(crate) fn parent_id(&self) -> Option<RunId> {
        self.run.parent_id
    }

    pub(crate) fn workflow_name(&self) -> Option<&str> {
        self.run.workflow.name.as_deref()
    }

    pub(crate) fn workflow_graph_name(&self) -> Option<&str> {
        self.run.workflow.graph_name.as_deref()
    }

    pub(crate) fn workflow_slug(&self) -> Option<&str> {
        self.run.workflow.slug.as_deref()
    }

    pub(crate) fn workflow_display_name(&self) -> String {
        self.workflow_name()
            .or_else(|| self.workflow_graph_name())
            .or_else(|| self.workflow_slug())
            .unwrap_or("-")
            .to_string()
    }

    pub(crate) fn workflow_matches(&self, pattern: &str) -> bool {
        [
            self.workflow_name(),
            self.workflow_graph_name(),
            self.workflow_slug(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.contains(pattern))
    }

    pub(crate) fn status(&self) -> RunStatus {
        self.run.lifecycle.status
    }

    pub(crate) fn start_time(&self) -> String {
        self.start_time_dt()
            .map(|time| time.to_rfc3339())
            .unwrap_or_default()
    }

    pub(crate) fn start_time_dt(&self) -> Option<DateTime<Utc>> {
        self.run
            .timestamps
            .started_at
            .or(Some(self.run.id.created_at()))
    }

    pub(crate) fn labels(&self) -> &HashMap<String, String> {
        &self.run.labels
    }

    pub(crate) fn duration_ms(&self) -> Option<u64> {
        self.run.timestamps.duration_ms
    }

    pub(crate) fn total_usd_micros(&self) -> Option<i64> {
        self.run
            .billing
            .as_ref()
            .and_then(|billing| billing.total_usd_micros)
    }

    pub(crate) fn source_directory(&self) -> Option<&str> {
        self.run.source_directory.as_deref()
    }

    pub(crate) fn repo_origin_url(&self) -> Option<&str> {
        self.run
            .repository
            .as_ref()
            .and_then(|repository| repository.origin_url.as_deref())
    }

    pub(crate) fn goal(&self) -> String {
        self.run.goal.clone()
    }
}

pub(crate) struct ServerRunLookup {
    runs: Vec<ServerRunInfo>,
}

impl ServerRunLookup {
    pub(crate) async fn from_client(client: Arc<Client>) -> Result<Self> {
        let runs = client.list_store_runs().await?;
        Ok(Self::from_runs(runs))
    }

    pub(crate) async fn from_client_by_parent(
        client: Arc<Client>,
        parent_id: RunId,
    ) -> Result<Self> {
        let runs = client.list_store_runs_by_parent(parent_id).await?;
        Ok(Self::from_runs(runs))
    }

    fn from_runs(runs: Vec<Run>) -> Self {
        let mut runs = runs
            .into_iter()
            .map(ServerRunInfo::from_run)
            .collect::<Vec<_>>();
        runs.sort_by(|a, b| {
            b.start_time_dt()
                .cmp(&a.start_time_dt())
                .then_with(|| b.run_id().cmp(&a.run_id()))
        });
        Self { runs }
    }

    pub(crate) fn runs(&self) -> &[ServerRunInfo] {
        &self.runs
    }
}

pub(crate) fn filter_server_runs(
    runs: &[ServerRunInfo],
    before: Option<&str>,
    workflow: Option<&str>,
    labels: &[(String, String)],
    running_only: bool,
) -> Vec<ServerRunInfo> {
    runs.iter()
        .filter(|run| !running_only || run.status().is_active())
        .filter(|run| {
            before.is_none_or(|before| {
                let start_time = run.start_time();
                start_time.is_empty() || start_time.as_str() < before
            })
        })
        .filter(|run| workflow.is_none_or(|pattern| run.workflow_matches(pattern)))
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
