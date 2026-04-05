use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use fabro_api::types;
use fabro_store::{EventEnvelope, RunSummary, StageId};
use fabro_types::{
    Checkpoint, Conclusion, NodeStatusRecord, PullRequestRecord, Retro, RunEvent, RunId,
    RunRecord, RunStatusRecord, SandboxRecord, Settings, StartRecord,
};

use crate::commands::server::start;

pub(crate) struct ServerStoreClient {
    client: fabro_api::Client,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct RunProjection {
    #[serde(default)]
    pub run: Option<RunRecord>,
    #[serde(default)]
    pub graph_source: Option<String>,
    #[serde(default)]
    pub start: Option<StartRecord>,
    #[serde(default)]
    pub status: Option<RunStatusRecord>,
    #[serde(default)]
    pub checkpoint: Option<Checkpoint>,
    #[serde(default)]
    pub checkpoints: Vec<(u32, Checkpoint)>,
    #[serde(default)]
    pub conclusion: Option<Conclusion>,
    #[serde(default)]
    pub retro: Option<Retro>,
    #[serde(default)]
    pub retro_prompt: Option<String>,
    #[serde(default)]
    pub retro_response: Option<String>,
    #[serde(default)]
    pub sandbox: Option<SandboxRecord>,
    #[serde(default)]
    pub final_patch: Option<String>,
    #[serde(default)]
    pub pull_request: Option<PullRequestRecord>,
    #[serde(default)]
    nodes: HashMap<String, NodeState>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct NodeState {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub status: Option<NodeStatusRecord>,
    #[serde(default)]
    pub provider_used: Option<serde_json::Value>,
    #[serde(default)]
    pub diff: Option<String>,
    #[serde(default)]
    pub script_invocation: Option<serde_json::Value>,
    #[serde(default)]
    pub script_timing: Option<serde_json::Value>,
    #[serde(default)]
    pub parallel_results: Option<serde_json::Value>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
}

impl RunProjection {
    pub(crate) fn list_node_visits(&self, node_id: &str) -> Vec<u32> {
        let mut visits = self
            .nodes
            .keys()
            .filter_map(|key| key.parse::<StageId>().ok())
            .filter(|stage_id| stage_id.node_id() == node_id)
            .map(|stage_id| stage_id.visit())
            .collect::<Vec<_>>();
        visits.sort_unstable();
        visits.dedup();
        visits
    }

    pub(crate) fn node(&self, stage_id: &StageId) -> Option<&NodeState> {
        self.nodes.get(&stage_id.to_string())
    }
}

pub(crate) async fn connect_server(storage_dir: &Path) -> Result<ServerStoreClient> {
    let bind = start::ensure_server_running(storage_dir)
        .with_context(|| format!("Failed to start fabro server for {}", storage_dir.display()))?;
    let socket_path = match bind {
        fabro_server::bind::Bind::Unix(path) => path,
        fabro_server::bind::Bind::Tcp(addr) => {
            return Err(anyhow!(
                "Unsupported server bind for store client auto-connect: {addr}"
            ));
        }
    };

    let http_client = reqwest::ClientBuilder::new()
        .unix_socket(socket_path)
        .no_proxy()
        .build()
        .context("Failed to build Unix-socket HTTP client for fabro server")?;
    wait_for_server_ready(&http_client).await?;

    Ok(ServerStoreClient {
        client: fabro_api::Client::new_with_client("http://fabro", http_client),
    })
}

async fn wait_for_server_ready(http_client: &reqwest::Client) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_error = None;

    while std::time::Instant::now() < deadline {
        match http_client.get("http://fabro/health").send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                last_error = Some(anyhow!(
                    "server health check returned status {}",
                    response.status()
                ));
            }
            Err(err) => last_error = Some(anyhow!(err)),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Err(last_error.unwrap_or_else(|| anyhow!("server did not become ready in time")))
}

impl ServerStoreClient {
    pub(crate) async fn create_run_from_workflow_path(
        &self,
        workflow_path: &Path,
        cwd: &Path,
        settings: &Settings,
        run_id: Option<&RunId>,
    ) -> Result<RunId> {
        let response = self
            .client
            .create_run()
            .body(types::CreateRunRequest {
                dot_source: None,
                workflow_path: Some(workflow_path.display().to_string()),
                cwd: Some(cwd.display().to_string()),
                settings_json: Some(serde_json::to_string(settings)?),
                run_id: run_id.map(ToString::to_string),
            })
            .send()
            .await
            .map_err(map_api_error)?;
        let status = response.into_inner();
        status
            .id
            .parse()
            .map_err(|err| anyhow!("invalid run ID from server: {err}"))
    }

    pub(crate) async fn start_run(&self, run_id: &RunId, resume: bool) -> Result<()> {
        self.client
            .start_run()
            .id(run_id.to_string())
            .body(types::StartRunRequest { resume })
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(())
    }

    pub(crate) async fn cancel_run(&self, run_id: &RunId) -> Result<()> {
        self.client
            .cancel_run()
            .id(run_id.to_string())
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(())
    }

    pub(crate) async fn list_store_runs(&self) -> Result<Vec<RunSummary>> {
        let response = self
            .client
            .list_runs()
            .send()
            .await
            .map_err(map_api_error)?;
        response
            .into_inner()
            .into_iter()
            .map(convert_type)
            .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn get_run_state(&self, run_id: &RunId) -> Result<RunProjection> {
        let response = self
            .client
            .get_run_state()
            .id(run_id.to_string())
            .send()
            .await
            .map_err(map_api_error)?;
        convert_type(response.into_inner())
    }

    pub(crate) async fn list_run_events(
        &self,
        run_id: &RunId,
        since_seq: Option<u32>,
        limit: Option<usize>,
    ) -> Result<Vec<EventEnvelope>> {
        let mut request = self.client.list_run_events().id(run_id.to_string());
        if let Some(seq) = since_seq.and_then(non_zero_u64_from_u32) {
            request = request.since_seq(seq);
        }
        if let Some(limit) = limit.and_then(non_zero_u64_from_usize) {
            request = request.limit(limit);
        }
        let response = request.send().await.map_err(map_api_error)?;
        response
            .into_inner()
            .data
            .into_iter()
            .map(convert_type)
            .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn append_run_event(&self, run_id: &RunId, event: &RunEvent) -> Result<()> {
        let body: types::RunEvent = convert_type(event)?;
        self.client
            .append_run_event()
            .id(run_id.to_string())
            .body(body)
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(())
    }

    pub(crate) async fn delete_store_run(&self, run_id: &RunId) -> Result<()> {
        self.client
            .delete_run()
            .id(run_id.to_string())
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(())
    }
}

fn map_api_error<E>(err: progenitor_client::Error<E>) -> anyhow::Error
where
    E: serde::Serialize + std::fmt::Debug,
{
    match err {
        progenitor_client::Error::ErrorResponse(response) => {
            let status = response.status();
            if let Ok(value) = serde_json::to_value(response.into_inner()) {
                if let Some(detail) = value
                    .get("errors")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|errors| errors.first())
                    .and_then(|entry| entry.get("detail"))
                    .and_then(serde_json::Value::as_str)
                {
                    return anyhow!("{detail}");
                }
            }
            anyhow!("request failed with status {status}")
        }
        progenitor_client::Error::UnexpectedResponse(response) => {
            anyhow!("request failed with status {}", response.status())
        }
        other => anyhow!("{other}"),
    }
}

fn convert_type<TInput, TOutput>(value: TInput) -> Result<TOutput>
where
    TInput: serde::Serialize,
    TOutput: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::to_value(value)?).map_err(Into::into)
}

fn non_zero_u64_from_u32(value: u32) -> Option<NonZeroU64> {
    NonZeroU64::new(u64::from(value))
}

fn non_zero_u64_from_usize(value: usize) -> Option<NonZeroU64> {
    u64::try_from(value).ok().and_then(NonZeroU64::new)
}
