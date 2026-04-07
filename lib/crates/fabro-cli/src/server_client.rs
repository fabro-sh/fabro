use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use bytes::Bytes;
use fabro_api::types;
use fabro_server::bind::Bind;
use fabro_store::{EventEnvelope, RunSummary, StageId};
use fabro_types::{RunBlobId, RunEvent, RunId, Settings};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use tokio::time::sleep;

use crate::args::ServerTargetArgs;
use crate::commands::server::start;
use crate::sse;
use crate::user_config;

#[derive(Clone)]
pub(crate) struct ServerStoreClient {
    client: fabro_api::Client,
}

#[derive(Debug, Clone)]
struct LocalServerRuntime {
    active_config_path: PathBuf,
    storage_dir: PathBuf,
}

pub(crate) struct RunAttachEventStream {
    stream: progenitor_client::ByteStream,
    pending_bytes: Vec<u8>,
    buffered_events: VecDeque<EventEnvelope>,
}

pub(crate) enum RunAttachStreamError {
    Gone,
    Other(anyhow::Error),
}

impl RunAttachEventStream {
    fn new(stream: progenitor_client::ByteStream) -> Self {
        Self {
            stream,
            pending_bytes: Vec::new(),
            buffered_events: VecDeque::new(),
        }
    }

    pub(crate) async fn next_event(&mut self) -> Result<Option<EventEnvelope>> {
        loop {
            if let Some(event) = self.buffered_events.pop_front() {
                return Ok(Some(event));
            }

            match self.stream.next().await {
                Some(chunk) => {
                    let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
                    self.pending_bytes.extend_from_slice(&chunk);
                    self.buffer_sse_events(false)?;
                }
                None => {
                    self.buffer_sse_events(true)?;
                    return Ok(self.buffered_events.pop_front());
                }
            }
        }
    }

    fn buffer_sse_events(&mut self, finalize: bool) -> Result<()> {
        for payload in sse::drain_sse_payloads(&mut self.pending_bytes, finalize) {
            let event: types::EventEnvelope = serde_json::from_str(&payload)?;
            self.buffered_events.push_back(convert_type(event)?);
        }
        Ok(())
    }
}

pub(crate) use fabro_store::RunProjection;

pub(crate) async fn connect_server(storage_dir: &Path) -> Result<ServerStoreClient> {
    Ok(ServerStoreClient {
        client: connect_api_client(storage_dir).await?,
    })
}

pub(crate) async fn connect_server_target_direct(target: &str) -> Result<ServerStoreClient> {
    let client = if target.starts_with("http://") || target.starts_with("https://") {
        connect_remote_api_client(target, None)?
    } else {
        let path = Path::new(target);
        if !path.is_absolute() {
            bail!("server target must be an http(s) URL or absolute Unix socket path");
        }
        connect_unix_socket_api_client(path).await?
    };
    Ok(ServerStoreClient { client })
}

pub(crate) async fn connect_server_only(args: &ServerTargetArgs) -> Result<ServerStoreClient> {
    let settings = user_config::load_settings()?;
    let target = user_config::resolve_server_target(args, &settings)?;
    let runtime = LocalServerRuntime {
        active_config_path: user_config::active_settings_path(None),
        storage_dir: settings.storage_dir(),
    };
    Ok(ServerStoreClient {
        client: connect_target_api_client(&target, &runtime).await?,
    })
}

pub(crate) async fn connect_api_client(storage_dir: &Path) -> Result<fabro_api::Client> {
    let config_path = user_config::active_settings_path(None);
    let bind = start::ensure_server_running_for_storage(storage_dir, &config_path)
        .with_context(|| format!("Failed to start fabro server for {}", storage_dir.display()))?;
    match bind {
        Bind::Unix(path) => connect_unix_socket_api_client(&path).await,
        Bind::Tcp(addr) => Err(anyhow!(
            "Unsupported server bind for store client auto-connect: {addr}"
        )),
    }
}

async fn connect_target_api_client(
    target: &user_config::ServerTarget,
    runtime: &LocalServerRuntime,
) -> Result<fabro_api::Client> {
    match target {
        user_config::ServerTarget::HttpUrl { api_url, tls } => {
            Ok(connect_remote_api_client(api_url, tls.as_ref())?)
        }
        user_config::ServerTarget::UnixSocket(path) => {
            if let Ok(client) = connect_unix_socket_api_client(path).await {
                Ok(client)
            } else {
                start::ensure_server_running_on_socket(
                    path,
                    &runtime.active_config_path,
                    &runtime.storage_dir,
                )
                .with_context(|| format!("Failed to start fabro server for {}", path.display()))?;
                connect_unix_socket_api_client(path).await
            }
        }
    }
}

pub(crate) async fn connect_server_backed_api_client(
    args: &ServerTargetArgs,
) -> Result<fabro_api::Client> {
    connect_server_backed_api_client_with_storage_dir(args, None).await
}

pub(crate) async fn connect_server_backed_api_client_with_storage_dir(
    args: &ServerTargetArgs,
    storage_dir: Option<&Path>,
) -> Result<fabro_api::Client> {
    let settings = user_config::load_settings_with_storage_dir(storage_dir)?;
    let target = user_config::resolve_server_target(args, &settings)?;
    let runtime = LocalServerRuntime {
        active_config_path: user_config::active_settings_path(None),
        storage_dir: settings.storage_dir(),
    };
    connect_target_api_client(&target, &runtime).await
}

pub(crate) fn connect_remote_api_client(
    api_url: &str,
    tls: Option<&user_config::ClientTlsSettings>,
) -> Result<fabro_api::Client> {
    let http_client = user_config::build_server_client(tls)?;
    let normalized = normalize_remote_server_target(api_url);
    Ok(fabro_api::Client::new_with_client(&normalized, http_client))
}

fn normalize_remote_server_target(api_url: &str) -> String {
    api_url
        .trim_end_matches('/')
        .strip_suffix("/api/v1")
        .unwrap_or(api_url.trim_end_matches('/'))
        .to_string()
}

pub(crate) async fn connect_unix_socket_api_client(path: &Path) -> Result<fabro_api::Client> {
    let http_client = crate::user_config::cli_http_client_builder()
        .unix_socket(path)
        .no_proxy()
        .build()
        .context("Failed to build Unix-socket HTTP client for fabro server")?;
    wait_for_server_ready(&http_client).await?;

    Ok(fabro_api::Client::new_with_client(
        "http://fabro",
        http_client,
    ))
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
        sleep(Duration::from_millis(50)).await;
    }

    Err(last_error.unwrap_or_else(|| anyhow!("server did not become ready in time")))
}

impl ServerStoreClient {
    pub(crate) fn clone_for_reuse(&self) -> Self {
        self.clone()
    }

    pub(crate) async fn retrieve_server_settings(&self) -> Result<Settings> {
        let response = self
            .client
            .retrieve_server_settings()
            .send()
            .await
            .map_err(map_api_error)?;
        convert_type(response.into_inner())
    }

    pub(crate) async fn create_run_from_manifest(
        &self,
        manifest: types::RunManifest,
    ) -> Result<RunId> {
        let response = self
            .client
            .create_run()
            .body(manifest)
            .send()
            .await
            .map_err(map_api_error)?;
        let status = response.into_inner();
        status
            .id
            .parse()
            .map_err(|err| anyhow!("invalid run ID from server: {err}"))
    }

    pub(crate) async fn run_preflight(
        &self,
        manifest: types::RunManifest,
    ) -> Result<types::PreflightResponse> {
        self.client
            .run_preflight()
            .body(manifest)
            .send()
            .await
            .map(progenitor_client::ResponseValue::into_inner)
            .map_err(map_api_error)
    }

    pub(crate) async fn render_workflow_graph(
        &self,
        request: types::RenderWorkflowGraphRequest,
    ) -> Result<Vec<u8>> {
        let response = self
            .client
            .render_workflow_graph()
            .body(request)
            .send()
            .await
            .map_err(map_api_error)?;
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
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

    pub(crate) async fn attach_run_events(
        &self,
        run_id: &RunId,
        since_seq: Option<u32>,
    ) -> std::result::Result<RunAttachEventStream, RunAttachStreamError> {
        let mut request = self.client.attach_run_events().id(run_id.to_string());
        if let Some(seq) = since_seq.and_then(non_zero_u64_from_u32) {
            request = request.since_seq(seq);
        }
        let response = request.send().await.map_err(map_attach_run_stream_error)?;
        Ok(RunAttachEventStream::new(response.into_inner()))
    }

    pub(crate) async fn list_run_questions(
        &self,
        run_id: &RunId,
    ) -> Result<Vec<types::ApiQuestion>> {
        let response = self
            .client
            .list_run_questions()
            .id(run_id.to_string())
            .page_limit(100)
            .page_offset(0)
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(response.into_inner().data)
    }

    pub(crate) async fn submit_run_answer(
        &self,
        run_id: &RunId,
        qid: &str,
        value: Option<String>,
        selected_option_key: Option<String>,
        selected_option_keys: Vec<String>,
    ) -> Result<()> {
        self.client
            .submit_run_answer()
            .id(run_id.to_string())
            .qid(qid)
            .body(types::SubmitAnswerRequest {
                value,
                selected_option_key,
                selected_option_keys,
            })
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(())
    }

    pub(crate) async fn append_run_event(&self, run_id: &RunId, event: &RunEvent) -> Result<u32> {
        let body: types::RunEvent = convert_type(event)?;
        let response = self
            .client
            .append_run_event()
            .id(run_id.to_string())
            .body(body)
            .send()
            .await
            .map_err(map_api_error)?;
        u32::try_from(response.into_inner().seq).context("append_run_event returned invalid seq")
    }

    pub(crate) async fn write_run_blob(&self, run_id: &RunId, data: &[u8]) -> Result<RunBlobId> {
        let response = self
            .client
            .write_run_blob()
            .id(run_id.to_string())
            .body(data.to_vec())
            .send()
            .await
            .map_err(map_api_error)?;
        response
            .into_inner()
            .id
            .parse()
            .context("write_run_blob returned invalid blob id")
    }

    pub(crate) async fn read_run_blob(
        &self,
        run_id: &RunId,
        blob_id: &RunBlobId,
    ) -> Result<Option<Bytes>> {
        let response = self
            .client
            .read_run_blob()
            .id(run_id.to_string())
            .blob_id(blob_id.to_string())
            .send()
            .await;
        match response {
            Ok(response) => {
                let mut stream = response.into_inner();
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
                    bytes.extend_from_slice(&chunk);
                }
                Ok(Some(Bytes::from(bytes)))
            }
            Err(err) => {
                if is_not_found_error(&err) {
                    Ok(None)
                } else {
                    Err(map_api_error(err))
                }
            }
        }
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

    pub(crate) async fn list_run_artifacts(
        &self,
        run_id: &RunId,
    ) -> Result<Vec<types::RunArtifactEntry>> {
        let response = self
            .client
            .list_run_artifacts()
            .id(run_id.to_string())
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(response.into_inner().data)
    }

    pub(crate) async fn download_stage_artifact(
        &self,
        run_id: &RunId,
        stage_id: &StageId,
        filename: &str,
    ) -> Result<Vec<u8>> {
        let response = self
            .client
            .get_stage_artifact()
            .id(run_id.to_string())
            .stage_id(stage_id.to_string())
            .filename(filename)
            .send()
            .await
            .map_err(map_api_error)?;
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub(crate) async fn generate_preview_url(
        &self,
        run_id: &RunId,
        port: u16,
        expires_in_secs: u64,
        signed: bool,
    ) -> Result<types::PreviewUrlResponse> {
        let expires_in_secs = NonZeroU64::new(expires_in_secs)
            .ok_or_else(|| anyhow!("preview expiry must be greater than zero"))?;
        let response = self
            .client
            .generate_preview_url()
            .id(run_id.to_string())
            .body(types::PreviewUrlRequest {
                expires_in_secs,
                port: i64::from(port),
                signed,
            })
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(response.into_inner())
    }

    pub(crate) async fn create_run_ssh_access(
        &self,
        run_id: &RunId,
        ttl_minutes: f64,
    ) -> Result<types::SshAccessResponse> {
        let response = self
            .client
            .create_run_ssh_access()
            .id(run_id.to_string())
            .body(types::SshAccessRequest { ttl_minutes })
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(response.into_inner())
    }

    pub(crate) async fn list_sandbox_files(
        &self,
        run_id: &RunId,
        path: &str,
        depth: Option<u32>,
    ) -> Result<Vec<types::SandboxFileEntry>> {
        let mut request = self
            .client
            .list_sandbox_files()
            .id(run_id.to_string())
            .path(path);
        if let Some(depth) = depth.and_then(non_zero_u64_from_u32) {
            request = request.depth(depth);
        }
        let response = request.send().await.map_err(map_api_error)?;
        Ok(response.into_inner().data)
    }

    pub(crate) async fn get_sandbox_file(&self, run_id: &RunId, path: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get_sandbox_file()
            .id(run_id.to_string())
            .path(path)
            .send()
            .await
            .map_err(map_api_error)?;
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub(crate) async fn put_sandbox_file(
        &self,
        run_id: &RunId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        self.client
            .put_sandbox_file()
            .id(run_id.to_string())
            .path(path)
            .body(bytes)
            .send()
            .await
            .map_err(map_api_error)?;
        Ok(())
    }
}

pub(crate) fn map_api_error<E>(err: progenitor_client::Error<E>) -> anyhow::Error
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

fn is_not_found_error<E>(err: &progenitor_client::Error<E>) -> bool
where
    E: serde::Serialize + std::fmt::Debug,
{
    match err {
        progenitor_client::Error::ErrorResponse(response) => {
            response.status() == reqwest::StatusCode::NOT_FOUND
        }
        progenitor_client::Error::UnexpectedResponse(response) => {
            response.status() == reqwest::StatusCode::NOT_FOUND
        }
        _ => false,
    }
}

fn map_attach_run_stream_error(
    err: progenitor_client::Error<types::ErrorResponse>,
) -> RunAttachStreamError {
    match &err {
        progenitor_client::Error::ErrorResponse(response)
            if response.status() == reqwest::StatusCode::GONE =>
        {
            RunAttachStreamError::Gone
        }
        _ => RunAttachStreamError::Other(map_api_error(err)),
    }
}

fn convert_type<TInput, TOutput>(value: TInput) -> Result<TOutput>
where
    TInput: serde::Serialize,
    TOutput: DeserializeOwned,
{
    serde_json::from_value(serde_json::to_value(value)?).map_err(Into::into)
}

fn non_zero_u64_from_u32(value: u32) -> Option<NonZeroU64> {
    NonZeroU64::new(u64::from(value))
}

fn non_zero_u64_from_usize(value: usize) -> Option<NonZeroU64> {
    u64::try_from(value).ok().and_then(NonZeroU64::new)
}
