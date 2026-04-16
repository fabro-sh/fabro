use std::collections::VecDeque;
use std::net::IpAddr;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use bytes::Bytes;
use fabro_api::types;
use fabro_config::Storage;
use fabro_http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use fabro_http::multipart::{Form, Part};
use fabro_server::bind::Bind;
use fabro_store::{EventEnvelope, RunSummary, StageId};
use fabro_types::settings::SettingsLayer;
use fabro_types::{RunBlobId, RunEvent, RunId};
use fabro_util::dev_token::validate_dev_token_format;
use fabro_util::{Home, dev_token};
use fabro_workflow::artifact_snapshot::CapturedArtifactInfo;
use futures::StreamExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::fs::File;
use tokio::time::sleep;
use tokio_util::io::ReaderStream;

use crate::args::ServerTargetArgs;
use crate::commands::server::{record, start};
use crate::user_config::cli_http_client_builder;
use crate::{sse, user_config};

#[derive(Clone)]
pub(crate) struct ServerStoreClient {
    client: fabro_api::Client,
    http_client: fabro_http::HttpClient,
    base_url: String,
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

            if let Some(chunk) = self.stream.next().await {
                let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
                self.pending_bytes.extend_from_slice(&chunk);
                self.buffer_sse_events(false)?;
            } else {
                self.buffer_sse_events(true)?;
                return Ok(self.buffered_events.pop_front());
            }
        }
    }

    fn buffer_sse_events(&mut self, finalize: bool) -> Result<()> {
        for payload in sse::drain_sse_payloads(&mut self.pending_bytes, finalize) {
            self.buffered_events
                .push_back(serde_json::from_str(&payload)?);
        }
        Ok(())
    }
}

pub(crate) use fabro_store::RunProjection;

pub(crate) async fn connect_server(storage_dir: &Path) -> Result<ServerStoreClient> {
    connect_api_client_bundle(storage_dir).await
}

pub(crate) async fn connect_server_target_direct(target: &str) -> Result<ServerStoreClient> {
    if target.starts_with("http://") || target.starts_with("https://") {
        connect_remote_api_client_bundle(target, None, RemoteDevTokenAuth::Ambient)
    } else {
        let path = Path::new(target);
        if !path.is_absolute() {
            bail!("server target must be an http(s) URL or absolute Unix socket path");
        }
        connect_unix_socket_api_client_bundle(path, None).await
    }
}

pub(crate) async fn connect_server_with_settings(
    args: &ServerTargetArgs,
    settings: &SettingsLayer,
    base_config_path: &Path,
) -> Result<ServerStoreClient> {
    let target = user_config::resolve_server_target(args, settings)?;
    let runtime = LocalServerRuntime {
        active_config_path: base_config_path.to_path_buf(),
        storage_dir: user_config::storage_dir(settings)?,
    };
    connect_target_api_client_bundle(&target, &runtime).await
}

async fn connect_api_client_bundle(storage_dir: &Path) -> Result<ServerStoreClient> {
    let config_path = user_config::active_settings_path(None);
    let bind = start::ensure_server_running_for_storage(storage_dir, &config_path)
        .await
        .with_context(|| format!("Failed to start fabro server for {}", storage_dir.display()))?;
    match bind {
        Bind::Unix(path) => connect_unix_socket_api_client_bundle(&path, Some(storage_dir)).await,
        Bind::Tcp(addr) => {
            let token = wait_for_local_dev_token(storage_dir).await?;
            let builder = cli_http_client_builder().no_proxy();
            let http_client = apply_bearer_token_auth(builder, &token)?.build()?;
            let base_url = format!("http://{addr}");
            let client = fabro_api::Client::new_with_client(&base_url, http_client.clone());
            Ok(ServerStoreClient {
                client,
                http_client,
                base_url,
            })
        }
    }
}

pub(crate) async fn connect_api_client(storage_dir: &Path) -> Result<fabro_api::Client> {
    connect_api_client_bundle(storage_dir)
        .await
        .map(|client| client.client)
}

async fn connect_target_api_client_bundle(
    target: &user_config::ServerTarget,
    runtime: &LocalServerRuntime,
) -> Result<ServerStoreClient> {
    match target {
        user_config::ServerTarget::HttpUrl { api_url, tls } => connect_remote_api_client_bundle(
            api_url,
            tls.as_ref(),
            remote_dev_token_auth_for_target(api_url, &runtime.storage_dir),
        ),
        user_config::ServerTarget::UnixSocket(path) => {
            if let Ok(client) =
                try_connect_unix_socket_api_client_bundle(path, Some(&runtime.storage_dir)).await
            {
                Ok(client)
            } else {
                start::ensure_server_running_on_socket(
                    path,
                    &runtime.active_config_path,
                    &runtime.storage_dir,
                )
                .await
                .with_context(|| format!("Failed to start fabro server for {}", path.display()))?;
                connect_unix_socket_api_client_bundle(path, Some(&runtime.storage_dir)).await
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RemoteDevTokenAuth<'a> {
    None,
    Storage(&'a Path),
    AmbientLocalTarget,
    Ambient,
}

fn connect_remote_api_client_bundle(
    api_url: &str,
    tls: Option<&user_config::ClientTlsSettings>,
    dev_token_auth: RemoteDevTokenAuth<'_>,
) -> Result<ServerStoreClient> {
    let normalized = normalize_remote_server_target(api_url);
    let mut builder = user_config::build_server_client_builder(tls)?;
    builder = match dev_token_auth {
        RemoteDevTokenAuth::None => builder,
        RemoteDevTokenAuth::Storage(storage_dir) => {
            apply_dev_token_auth(builder.no_proxy(), Some(storage_dir))?
        }
        RemoteDevTokenAuth::AmbientLocalTarget => {
            if remote_url_targets_local_host(&normalized) {
                apply_dev_token_auth(builder.no_proxy(), None)?
            } else {
                builder
            }
        }
        RemoteDevTokenAuth::Ambient => apply_dev_token_auth(builder, None)?,
    };
    let http_client = builder.build()?;
    let client = fabro_api::Client::new_with_client(&normalized, http_client.clone());
    Ok(ServerStoreClient {
        client,
        http_client,
        base_url: normalized,
    })
}

fn normalize_remote_server_target(api_url: &str) -> String {
    api_url
        .trim_end_matches('/')
        .strip_suffix("/api/v1")
        .unwrap_or(api_url.trim_end_matches('/'))
        .to_string()
}

fn remote_dev_token_auth_for_target<'a>(
    api_url: &str,
    storage_dir: &'a Path,
) -> RemoteDevTokenAuth<'a> {
    if remote_url_matches_active_local_tcp_server(api_url, storage_dir) {
        RemoteDevTokenAuth::Storage(storage_dir)
    } else if remote_url_targets_local_host(api_url) {
        RemoteDevTokenAuth::AmbientLocalTarget
    } else {
        RemoteDevTokenAuth::None
    }
}

fn remote_url_matches_active_local_tcp_server(api_url: &str, storage_dir: &Path) -> bool {
    let Some(record) = record::active_server_record(storage_dir) else {
        return false;
    };
    let Bind::Tcp(bind_addr) = record.bind else {
        return false;
    };
    remote_url_matches_tcp_bind(api_url, bind_addr)
}

fn remote_url_matches_tcp_bind(api_url: &str, bind_addr: std::net::SocketAddr) -> bool {
    let Ok(url) = fabro_http::Url::parse(&normalize_remote_server_target(api_url)) else {
        return false;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    if port != bind_addr.port() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host_matches_bind(host, bind_addr.ip())
}

fn remote_url_targets_local_host(api_url: &str) -> bool {
    let Ok(url) = fabro_http::Url::parse(&normalize_remote_server_target(api_url)) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    host_is_local(host)
}

fn host_matches_bind(host: &str, bind_ip: IpAddr) -> bool {
    host.parse::<IpAddr>()
        .ok()
        .is_some_and(|host_ip| host_ip == bind_ip)
        || (host_is_local(host) && (bind_ip.is_loopback() || bind_ip.is_unspecified()))
}

fn host_is_local(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .ok()
            .is_some_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

fn load_dev_token_if_available(storage_dir: Option<&Path>) -> Option<String> {
    if let Some(token) = std::env::var("FABRO_DEV_TOKEN")
        .ok()
        .filter(|token| validate_dev_token_format(token))
    {
        return Some(token);
    }

    if let Some(storage_dir) = storage_dir {
        let storage_token_path = Storage::new(storage_dir).server_state().dev_token_path();
        if let Some(token) = dev_token::read_dev_token_file(&storage_token_path) {
            return Some(token);
        }

        let record_path = Storage::new(storage_dir).server_state().record_path();
        if let Some(token) = record::read_server_record(&record_path)
            .and_then(|server| server.dev_token_path)
            .as_deref()
            .and_then(dev_token::read_dev_token_file)
        {
            return Some(token);
        }
    }

    dev_token::read_dev_token_file(&Home::from_env().dev_token_path())
}

async fn wait_for_local_dev_token(storage_dir: &Path) -> Result<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    while std::time::Instant::now() < deadline {
        if let Some(token) = load_dev_token_if_available(Some(storage_dir)) {
            return Ok(token);
        }
        sleep(Duration::from_millis(50)).await;
    }

    bail!(
        "local server dev token did not become available for {}",
        storage_dir.display()
    );
}

fn apply_bearer_token_auth(
    builder: fabro_http::HttpClientBuilder,
    token: &str,
) -> Result<fabro_http::HttpClientBuilder> {
    let mut headers = fabro_http::HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        fabro_http::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid dev token header value")?,
    );
    Ok(builder.default_headers(headers))
}

fn apply_dev_token_auth(
    builder: fabro_http::HttpClientBuilder,
    storage_dir: Option<&Path>,
) -> Result<fabro_http::HttpClientBuilder> {
    let Some(token) = load_dev_token_if_available(storage_dir) else {
        return Ok(builder);
    };
    apply_bearer_token_auth(builder, &token)
}

fn unix_socket_api_client_bundle(http_client: fabro_http::HttpClient) -> ServerStoreClient {
    let base_url = "http://fabro".to_string();
    let client = fabro_api::Client::new_with_client(&base_url, http_client.clone());
    ServerStoreClient {
        client,
        http_client,
        base_url,
    }
}

async fn build_authed_unix_socket_client(
    path: &Path,
    storage_dir: Option<&Path>,
) -> Result<ServerStoreClient> {
    let http_client = if let Some(storage_dir) = storage_dir {
        let token = wait_for_local_dev_token(storage_dir).await?;
        apply_bearer_token_auth(
            cli_http_client_builder().unix_socket(path).no_proxy(),
            &token,
        )?
        .build()
        .context("Failed to build Unix-socket HTTP client for fabro server")?
    } else {
        apply_dev_token_auth(cli_http_client_builder().unix_socket(path).no_proxy(), None)?
            .build()
            .context("Failed to build Unix-socket HTTP client for fabro server")?
    };
    Ok(unix_socket_api_client_bundle(http_client))
}

fn build_unix_socket_probe_client(path: &Path) -> Result<fabro_http::HttpClient> {
    cli_http_client_builder()
        .unix_socket(path)
        .no_proxy()
        .build()
        .context("Failed to build Unix-socket HTTP client for fabro server")
}

async fn try_connect_unix_socket_api_client_bundle(
    path: &Path,
    storage_dir: Option<&Path>,
) -> Result<ServerStoreClient> {
    check_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_client(path, storage_dir).await
}

async fn connect_unix_socket_api_client_bundle(
    path: &Path,
    storage_dir: Option<&Path>,
) -> Result<ServerStoreClient> {
    wait_for_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_client(path, storage_dir).await
}

async fn check_server_ready(http_client: &fabro_http::HttpClient) -> Result<()> {
    match http_client.get("http://fabro/health").send().await {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => bail!("server health check returned status {}", response.status()),
        Err(err) => Err(anyhow!(err)),
    }
}

async fn wait_for_server_ready(http_client: &fabro_http::HttpClient) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_error = None;

    while std::time::Instant::now() < deadline {
        match check_server_ready(http_client).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
            }
        }
        sleep(Duration::from_millis(50)).await;
    }

    Err(last_error.unwrap_or_else(|| anyhow!("server did not become ready in time")))
}

#[derive(Debug, Serialize)]
struct ArtifactBatchUploadManifest {
    entries: Vec<ArtifactBatchUploadEntry>,
}

#[derive(Debug, Serialize)]
struct ArtifactBatchUploadEntry {
    part: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
}

impl ServerStoreClient {
    /// Build a client for tests that bypasses proxy discovery.
    #[cfg(test)]
    pub(crate) fn new_no_proxy(base_url: &str) -> Result<Self> {
        let http_client = cli_http_client_builder().no_proxy().build()?;
        let client = fabro_api::Client::new_with_client(base_url, http_client.clone());
        Ok(Self {
            client,
            http_client,
            base_url: base_url.to_string(),
        })
    }

    pub(crate) fn clone_for_reuse(&self) -> Self {
        self.clone()
    }

    pub(crate) fn api(&self) -> &fabro_api::Client {
        &self.client
    }

    #[allow(dead_code)]
    pub(crate) fn http_client(&self) -> &fabro_http::HttpClient {
        &self.http_client
    }

    #[allow(dead_code)]
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) async fn retrieve_resolved_server_settings(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/v1/settings?view=resolved", self.base_url);
        let response = self.http_client.get(&url).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(detail) = value
                    .get("errors")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|errors| errors.first())
                    .and_then(|entry| entry.get("detail"))
                    .and_then(serde_json::Value::as_str)
                {
                    bail!("{detail}");
                }
            }
            if body.is_empty() {
                bail!("request failed with status {status}");
            }
            bail!("request failed with status {status}: {body}");
        }

        let marker = response
            .headers()
            .get("x-fabro-settings-view")
            .and_then(|value| value.to_str().ok());
        if marker != Some("resolved") {
            bail!(
                "server does not support resolved settings view; upgrade the server or use --local"
            );
        }

        response
            .json::<serde_json::Value>()
            .await
            .context("server returned invalid JSON for the resolved settings view")
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
        let mut next_since_seq = since_seq;
        let mut all_events = Vec::new();

        loop {
            let mut request = self.client.list_run_events().id(run_id.to_string());
            if let Some(seq) = next_since_seq.and_then(non_zero_u64_from_u32) {
                request = request.since_seq(seq);
            }
            if let Some(limit) = limit.and_then(non_zero_u64_from_usize) {
                request = request.limit(limit);
            }

            let response = request.send().await.map_err(map_api_error)?;
            let parsed = response.into_inner();
            let page_events = parsed
                .data
                .into_iter()
                .map(convert_type::<_, EventEnvelope>)
                .collect::<Result<Vec<EventEnvelope>>>()?;
            let next_page_since_seq = page_events.last().map(|event| event.seq.saturating_add(1));
            all_events.extend(page_events);

            if limit.is_some() || !parsed.meta.has_more || next_page_since_seq.is_none() {
                break;
            }
            next_since_seq = next_page_since_seq;
        }

        Ok(all_events)
    }

    pub(crate) async fn attach_run_events(
        &self,
        run_id: &RunId,
        since_seq: Option<u32>,
    ) -> Result<RunAttachEventStream> {
        let mut request = self.client.attach_run_events().id(run_id.to_string());
        if let Some(seq) = since_seq.and_then(non_zero_u64_from_u32) {
            request = request.since_seq(seq);
        }
        let response = request.send().await.map_err(map_api_error)?;
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

    fn stage_artifacts_url(&self, run_id: &RunId, stage_id: &StageId) -> Result<fabro_http::Url> {
        let mut url = fabro_http::Url::parse(&self.base_url)
            .with_context(|| format!("invalid server base URL {}", self.base_url))?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("server base URL cannot accept path segments"))?
            .extend([
                "api",
                "v1",
                "runs",
                &run_id.to_string(),
                "stages",
                &stage_id.to_string(),
                "artifacts",
            ]);
        Ok(url)
    }

    pub(crate) async fn upload_stage_artifact_file(
        &self,
        run_id: &RunId,
        stage_id: &StageId,
        filename: &str,
        path: &Path,
        bearer_token: &str,
    ) -> Result<()> {
        let mut url = self.stage_artifacts_url(run_id, stage_id)?;
        url.query_pairs_mut().append_pair("filename", filename);

        let file = File::open(path)
            .await
            .with_context(|| format!("failed to open artifact {}", path.display()))?;
        let content_length = file
            .metadata()
            .await
            .with_context(|| format!("failed to stat artifact {}", path.display()))?
            .len();
        let body = fabro_http::Body::wrap_stream(ReaderStream::new(file));

        let response = self
            .http_client
            .post(url)
            .bearer_auth(bearer_token)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, content_length.to_string())
            .body(body)
            .send()
            .await
            .with_context(|| format!("failed to upload artifact {}", path.display()))?;
        ensure_raw_response_success(response).await
    }

    pub(crate) async fn upload_stage_artifact_batch(
        &self,
        run_id: &RunId,
        stage_id: &StageId,
        artifact_capture_dir: &Path,
        artifacts: &[CapturedArtifactInfo],
        bearer_token: &str,
    ) -> Result<()> {
        let url = self.stage_artifacts_url(run_id, stage_id)?;
        let mut manifest_entries = Vec::with_capacity(artifacts.len());
        let mut file_parts = Vec::with_capacity(artifacts.len());

        for (index, artifact) in artifacts.iter().enumerate() {
            let part_name = format!("file{}", index + 1);
            let path = artifact_capture_dir.join(&artifact.path);
            let file = File::open(&path)
                .await
                .with_context(|| format!("failed to open artifact {}", path.display()))?;
            let content_length = file
                .metadata()
                .await
                .with_context(|| format!("failed to stat artifact {}", path.display()))?
                .len();

            manifest_entries.push(ArtifactBatchUploadEntry {
                part: part_name.clone(),
                path: artifact.path.clone(),
                sha256: Some(artifact.content_sha256.clone()),
                expected_bytes: Some(artifact.bytes),
                content_type: Some(artifact.mime.clone()),
            });

            file_parts.push((
                part_name,
                Part::stream_with_length(
                    fabro_http::Body::wrap_stream(ReaderStream::new(file)),
                    content_length,
                )
                .file_name(artifact.path.clone()),
            ));
        }

        let manifest = ArtifactBatchUploadManifest {
            entries: manifest_entries,
        };
        let manifest_part =
            Part::text(serde_json::to_string(&manifest)?).mime_str("application/json")?;
        let mut form = Form::new().part("manifest", manifest_part);
        for (part_name, part) in file_parts {
            form = form.part(part_name, part);
        }

        let response = self
            .http_client
            .post(url)
            .bearer_auth(bearer_token)
            .multipart(form)
            .send()
            .await
            .context("failed to upload artifact batch")?;
        ensure_raw_response_success(response).await
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

async fn ensure_raw_response_success(response: fabro_http::Response) -> Result<()> {
    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(detail) = value
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .and_then(|errors| errors.first())
            .and_then(|entry| entry.get("detail"))
            .and_then(serde_json::Value::as_str)
        {
            bail!("{detail}");
        }
    }

    if body.is_empty() {
        bail!("request failed with status {status}");
    }

    bail!("request failed with status {status}: {body}");
}

fn is_not_found_error<E>(err: &progenitor_client::Error<E>) -> bool
where
    E: serde::Serialize + std::fmt::Debug,
{
    match err {
        progenitor_client::Error::ErrorResponse(response) => {
            response.status() == fabro_http::StatusCode::NOT_FOUND
        }
        progenitor_client::Error::UnexpectedResponse(response) => {
            response.status() == fabro_http::StatusCode::NOT_FOUND
        }
        _ => false,
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

#[cfg(test)]
mod tests {
    use std::sync::{LazyLock, Mutex};

    use super::*;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn load_dev_token_if_available_prefers_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_home = tempfile::tempdir().unwrap();
        let token_path = temp_home.path().join("dev-token");
        std::fs::write(
            &token_path,
            "fabro_dev_abababababababababababababababababababababababababababababababab",
        )
        .unwrap();

        std::env::set_var("FABRO_HOME", temp_home.path());
        std::env::set_var(
            "FABRO_DEV_TOKEN",
            "fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        );

        let token = load_dev_token_if_available(None);

        std::env::remove_var("FABRO_DEV_TOKEN");
        std::env::remove_var("FABRO_HOME");

        assert_eq!(
            token.as_deref(),
            Some("fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd")
        );
    }

    #[test]
    fn load_dev_token_if_available_reads_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_home = tempfile::tempdir().unwrap();
        let token = "fabro_dev_abababababababababababababababababababababababababababababababab";
        std::fs::write(temp_home.path().join("dev-token"), token).unwrap();

        std::env::remove_var("FABRO_DEV_TOKEN");
        std::env::set_var("FABRO_HOME", temp_home.path());

        let loaded = load_dev_token_if_available(None);

        std::env::remove_var("FABRO_HOME");

        assert_eq!(loaded.as_deref(), Some(token));
    }

    #[test]
    fn load_dev_token_if_available_reads_path_from_active_server_record() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_home = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let token_dir = tempfile::tempdir().unwrap();
        let token = "fabro_dev_abababababababababababababababababababababababababababababababab";
        let token_path = token_dir.path().join("dev-token");
        std::fs::write(&token_path, token).unwrap();

        let record_path = fabro_config::Storage::new(storage.path())
            .server_state()
            .record_path();
        record::write_server_record(
            &record_path,
            &record::ServerRecord {
                pid: std::process::id(),
                bind: Bind::Unix(temp_home.path().join("fabro.sock")),
                log_path: storage.path().join("server.log"),
                dev_token_path: Some(token_path),
                started_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        std::env::remove_var("FABRO_DEV_TOKEN");
        std::env::set_var("FABRO_HOME", temp_home.path());

        let loaded = load_dev_token_if_available(Some(storage.path()));

        std::env::remove_var("FABRO_HOME");

        assert_eq!(loaded.as_deref(), Some(token));
    }

    #[test]
    fn remote_url_matches_tcp_bind_accepts_loopback_aliases() {
        let bind_addr = "127.0.0.1:32276".parse().unwrap();

        assert!(remote_url_matches_tcp_bind(
            "http://127.0.0.1:32276/api/v1",
            bind_addr
        ));
        assert!(remote_url_matches_tcp_bind(
            "http://localhost:32276",
            bind_addr
        ));
        assert!(!remote_url_matches_tcp_bind(
            "http://127.0.0.1:32277",
            bind_addr
        ));
    }

    #[test]
    fn remote_url_matches_tcp_bind_accepts_loopback_target_for_unspecified_bind() {
        let bind_addr = "0.0.0.0:32276".parse().unwrap();

        assert!(remote_url_matches_tcp_bind(
            "http://127.0.0.1:32276",
            bind_addr
        ));
        assert!(remote_url_matches_tcp_bind(
            "http://localhost:32276/api/v1",
            bind_addr
        ));
    }

    #[test]
    fn remote_url_targets_local_host_detects_local_http_urls() {
        assert!(remote_url_targets_local_host("http://127.0.0.1:32276"));
        assert!(remote_url_targets_local_host(
            "http://localhost:32276/api/v1"
        ));
        assert!(remote_url_targets_local_host("http://0.0.0.0:32276"));
        assert!(!remote_url_targets_local_host("https://example.com"));
    }
}
