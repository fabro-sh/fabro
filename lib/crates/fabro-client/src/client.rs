use std::collections::VecDeque;
use std::future::Future;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, RwLock};

use anyhow::{Context as _, Result, anyhow, bail};
use bytes::Bytes;
use fabro_api::types;
use fabro_http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use fabro_http::multipart::{Form, Part};
use fabro_model::Model;
use fabro_types::settings::run::MergeStrategy;
use fabro_types::{
    ArtifactUpload, EventEnvelope, RunBlobId, RunEvent, RunId, RunProjection, RunSummary, StageId,
};
use fabro_util::exit::{ErrorExt, ExitClass};
use futures::StreamExt;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio::sync::Mutex;
use tokio_util::io::ReaderStream;

use crate::credential::Credential;
use crate::error::{
    ApiError, ApiFailure, api_failure_for, classify_api_error, classify_http_response,
    convert_type, is_not_found_error, map_api_error, raw_response_failure_error,
};
use crate::session::OAuthSession;
use crate::target::ServerTarget;
use crate::{AuthEntry, OAuthEntry, StoredSubject, sse};

type TransportFuture = BoxFuture<'static, Result<(fabro_http::HttpClient, String)>>;

pub struct RunEventStream {
    stream:          progenitor_client::ByteStream,
    pending_bytes:   Vec<u8>,
    buffered_events: VecDeque<EventEnvelope>,
}

pub struct RewindRunResult {
    pub status:   u16,
    pub response: types::RewindResponse,
}

#[derive(Clone)]
struct ClientState {
    client:       fabro_api::ApiClient,
    http_client:  fabro_http::HttpClient,
    bearer_token: Option<String>,
    base_url:     String,
}

#[derive(Clone)]
pub struct Client {
    state:               Arc<RwLock<ClientState>>,
    oauth_session:       Option<OAuthSession>,
    refresh_lock:        Arc<Mutex<()>>,
    transport_connector: Option<TransportConnector>,
}

#[derive(Clone)]
pub struct TransportConnector {
    connect: Arc<dyn Fn(Option<String>) -> TransportFuture + Send + Sync>,
}

#[derive(Default)]
pub struct ClientBuilder {
    target:              Option<ServerTarget>,
    credential:          Option<Credential>,
    oauth_session:       Option<OAuthSession>,
    transport:           Option<(String, fabro_http::HttpClient)>,
    transport_connector: Option<TransportConnector>,
}

#[derive(Debug, Deserialize)]
struct CliTokenResponse {
    access_token:             String,
    access_token_expires_at:  chrono::DateTime<chrono::Utc>,
    refresh_token:            String,
    refresh_token_expires_at: chrono::DateTime<chrono::Utc>,
    subject:                  CliTokenSubject,
}

#[derive(Debug, Deserialize)]
struct CliTokenSubject {
    idp_issuer:  String,
    idp_subject: String,
    login:       String,
    name:        String,
    email:       String,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    error:             String,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Serialize)]
struct ArtifactBatchUploadManifest {
    entries: Vec<ArtifactBatchUploadEntry>,
}

#[derive(Debug, Serialize)]
struct ArtifactBatchUploadEntry {
    part:           String,
    path:           String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256:         Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type:   Option<String>,
}

impl RunEventStream {
    #[must_use]
    pub fn new(stream: progenitor_client::ByteStream) -> Self {
        Self {
            stream,
            pending_bytes: Vec::new(),
            buffered_events: VecDeque::new(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<EventEnvelope>> {
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

impl TransportConnector {
    pub fn new<F, Fut>(connect: F) -> Self
    where
        F: Fn(Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(fabro_http::HttpClient, String)>> + Send + 'static,
    {
        Self {
            connect: Arc::new(move |bearer_token| Box::pin(connect(bearer_token))),
        }
    }

    pub async fn connect(
        &self,
        bearer_token: Option<String>,
    ) -> Result<(fabro_http::HttpClient, String)> {
        (self.connect)(bearer_token).await
    }
}

impl ClientBuilder {
    #[must_use]
    pub fn target(mut self, target: ServerTarget) -> Self {
        self.target = Some(target);
        self
    }

    #[must_use]
    pub fn credential(mut self, credential: Credential) -> Self {
        self.credential = Some(credential);
        self
    }

    #[must_use]
    pub fn oauth_session(mut self, oauth_session: OAuthSession) -> Self {
        self.oauth_session = Some(oauth_session);
        self
    }

    #[must_use]
    pub fn transport(
        mut self,
        base_url: impl Into<String>,
        http_client: fabro_http::HttpClient,
    ) -> Self {
        self.transport = Some((base_url.into(), http_client));
        self
    }

    #[must_use]
    pub fn transport_connector(mut self, transport_connector: TransportConnector) -> Self {
        self.transport_connector = Some(transport_connector);
        self
    }

    pub async fn connect(self) -> Result<Client> {
        let bearer_token = self
            .credential
            .as_ref()
            .map(Credential::bearer_token)
            .map(ToOwned::to_owned);
        let target = self.target.clone().or_else(|| {
            self.oauth_session
                .as_ref()
                .map(|session| session.target.clone())
        });
        let transport_connector = self
            .transport_connector
            .or_else(|| target.map(default_transport_connector));

        let state = if let Some((base_url, http_client)) = self.transport {
            client_state(base_url, http_client, bearer_token.clone())
        } else {
            let Some(transport_connector) = transport_connector.clone() else {
                bail!("client builder requires a target, transport, or transport connector");
            };
            let (http_client, base_url) = transport_connector.connect(bearer_token.clone()).await?;
            client_state(base_url, http_client, bearer_token.clone())
        };

        Ok(Client {
            state: Arc::new(RwLock::new(state)),
            oauth_session: self.oauth_session,
            refresh_lock: Arc::new(Mutex::new(())),
            transport_connector,
        })
    }
}

impl Client {
    #[must_use]
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    #[must_use]
    pub fn from_http_client(
        base_url: impl Into<String>,
        http_client: fabro_http::HttpClient,
    ) -> Self {
        Self {
            state:               Arc::new(RwLock::new(client_state(
                base_url.into(),
                http_client,
                None,
            ))),
            oauth_session:       None,
            refresh_lock:        Arc::new(Mutex::new(())),
            transport_connector: None,
        }
    }

    pub fn new_no_proxy(base_url: &str) -> Result<Self> {
        let http_client = fabro_http::HttpClientBuilder::new().no_proxy().build()?;
        Ok(Self::from_http_client(base_url.to_string(), http_client))
    }

    #[must_use]
    pub fn clone_for_reuse(&self) -> Self {
        self.clone()
    }

    #[must_use]
    pub fn api_client(&self) -> fabro_api::ApiClient {
        self.current_state().client
    }

    #[must_use]
    pub fn http_client(&self) -> fabro_http::HttpClient {
        self.current_state().http_client
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        self.current_state().base_url
    }

    fn current_state(&self) -> ClientState {
        self.state
            .read()
            .expect("client state lock should not be poisoned")
            .clone()
    }

    fn replace_state(&self, state: ClientState) {
        *self
            .state
            .write()
            .expect("client state lock should not be poisoned") = state;
    }

    async fn send_api<T, E, F, Fut>(
        &self,
        request: F,
    ) -> Result<progenitor_client::ResponseValue<T>>
    where
        F: FnOnce(fabro_api::ApiClient) -> Fut + Clone,
        Fut: Future<
            Output = std::result::Result<
                progenitor_client::ResponseValue<T>,
                progenitor_client::Error<E>,
            >,
        >,
        E: serde::Serialize + std::fmt::Debug,
    {
        let state = self.current_state();
        match request.clone()(state.client.clone()).await {
            Ok(response) => Ok(response),
            Err(err) => {
                let mapped = classify_api_error(err).await;
                if self.should_refresh(mapped.failure.as_ref()) {
                    if let Some(failed_token) = state.bearer_token.as_deref() {
                        self.refresh_access_token(failed_token).await?;
                        let state = self.current_state();
                        return request(state.client.clone()).await.map_err(map_api_error);
                    }
                }
                Err(mapped.error)
            }
        }
    }

    fn should_refresh(&self, failure: Option<&ApiFailure>) -> bool {
        self.oauth_session.is_some()
            && failure.is_some_and(|failure| {
                failure.status == fabro_http::StatusCode::UNAUTHORIZED
                    && failure.code.as_deref() == Some("access_token_expired")
            })
    }

    async fn refresh_access_token(&self, failed_access_token: &str) -> Result<()> {
        fn session_expired() -> anyhow::Error {
            anyhow!("CLI session has expired. Run `fabro auth login` again.")
                .classify(ExitClass::AuthRequired)
        }

        let Some(oauth_session) = &self.oauth_session else {
            return Err(session_expired());
        };

        let _guard = self.refresh_lock.lock().await;
        let current_state = self.current_state();
        if current_state.bearer_token.as_deref() != Some(failed_access_token) {
            return Ok(());
        }

        let Some(entry) = oauth_session.auth_store.get(&oauth_session.target)? else {
            self.rebuild_with_fallback(oauth_session).await?;
            return Err(session_expired());
        };
        let oauth_entry = match entry {
            AuthEntry::DevToken(entry) => {
                self.rebuild_client(Some(entry.token)).await?;
                return Ok(());
            }
            AuthEntry::OAuth(entry) => entry,
        };
        if oauth_entry.refresh_token_expires_at <= chrono::Utc::now() {
            oauth_session.auth_store.remove(&oauth_session.target)?;
            self.rebuild_with_fallback(oauth_session).await?;
            return Err(session_expired());
        }
        let (http_client, base_url) = oauth_session.target.build_public_http_client()?;
        let response = http_client
            .post(format!("{base_url}/auth/cli/refresh"))
            .header(
                AUTHORIZATION,
                format!("Bearer {}", oauth_entry.refresh_token),
            )
            .send()
            .await?;

        if response.status().is_success() {
            let tokens = response
                .json::<CliTokenResponse>()
                .await
                .context("failed to parse CLI auth refresh response")?;
            let entry = OAuthEntry {
                access_token:             tokens.access_token.clone(),
                access_token_expires_at:  tokens.access_token_expires_at,
                refresh_token:            tokens.refresh_token.clone(),
                refresh_token_expires_at: tokens.refresh_token_expires_at,
                subject:                  StoredSubject {
                    idp_issuer:  tokens.subject.idp_issuer,
                    idp_subject: tokens.subject.idp_subject,
                    login:       tokens.subject.login,
                    name:        tokens.subject.name,
                    email:       tokens.subject.email,
                },
                logged_in_at:             oauth_entry.logged_in_at,
            };
            oauth_session
                .auth_store
                .put(&oauth_session.target, AuthEntry::OAuth(entry.clone()))
                .context("failed to persist refreshed CLI auth tokens")?;
            self.rebuild_client(Some(entry.access_token)).await?;
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let parsed_error = serde_json::from_str::<OAuthErrorBody>(&body).ok();
        let auth_recoverable = parsed_error.as_ref().is_some_and(|error| {
            matches!(
                error.error.as_str(),
                "refresh_token_expired" | "refresh_token_revoked"
            )
        });
        if auth_recoverable {
            oauth_session.auth_store.remove(&oauth_session.target)?;
            self.rebuild_with_fallback(oauth_session).await?;
        }

        let err = if let Some(parsed_error) = parsed_error {
            let message = parsed_error
                .error_description
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("request failed with status {status}"));
            anyhow!("{message}")
        } else if body.is_empty() {
            anyhow!("request failed with status {status}")
        } else {
            anyhow!("request failed with status {status}: {body}")
        };

        Err(if auth_recoverable {
            err.classify(ExitClass::AuthRequired)
        } else {
            err
        })
    }

    async fn rebuild_with_fallback(&self, oauth_session: &OAuthSession) -> Result<()> {
        let credential = oauth_session.resolve_fallback();
        self.rebuild_client(
            credential
                .as_ref()
                .map(Credential::bearer_token)
                .map(ToOwned::to_owned),
        )
        .await
    }

    async fn rebuild_client(&self, bearer_token: Option<String>) -> Result<()> {
        let Some(transport_connector) = &self.transport_connector else {
            bail!("client transport cannot be rebuilt");
        };
        let (http_client, base_url) = transport_connector.connect(bearer_token.clone()).await?;
        self.replace_state(client_state(base_url, http_client, bearer_token));
        Ok(())
    }

    pub async fn send_http_response<T, F, Fut>(
        &self,
        request: F,
    ) -> Result<std::result::Result<fabro_http::Response, ApiError>>
    where
        F: FnOnce(fabro_http::HttpClient) -> Fut + Clone,
        Fut: Future<Output = std::result::Result<fabro_http::Response, T>>,
        T: Into<anyhow::Error>,
    {
        let state = self.current_state();
        let response = request.clone()(state.http_client.clone())
            .await
            .map_err(Into::into)?;
        match classify_http_response(response).await? {
            Ok(response) => Ok(Ok(response)),
            Err(failure) => {
                if self.should_refresh(Some(failure.api_failure())) {
                    if let Some(failed_token) = state.bearer_token.as_deref() {
                        self.refresh_access_token(failed_token).await?;
                        let state = self.current_state();
                        let response = request(state.http_client.clone())
                            .await
                            .map_err(Into::into)?;
                        return classify_http_response(response).await;
                    }
                }
                Ok(Err(failure))
            }
        }
    }

    async fn send_http<T, F, Fut>(&self, request: F) -> Result<fabro_http::Response>
    where
        F: FnOnce(fabro_http::HttpClient) -> Fut + Clone,
        Fut: Future<Output = std::result::Result<fabro_http::Response, T>>,
        T: Into<anyhow::Error>,
    {
        match self.send_http_response(request).await? {
            Ok(response) => Ok(response),
            Err(failure) => Err(raw_response_failure_error(&failure)),
        }
    }

    pub async fn retrieve_resolved_server_settings(&self) -> Result<types::ServerSettings> {
        let url = format!("{}/api/v1/settings", self.base_url());
        let response = self
            .send_http(|http_client| async move { http_client.get(&url).send().await })
            .await?;

        response
            .json::<types::ServerSettings>()
            .await
            .context("server returned invalid JSON for server settings")
    }

    pub async fn create_run_from_manifest(&self, manifest: types::RunManifest) -> Result<RunId> {
        let response = self
            .send_api(
                |client| async move { client.create_run().body(manifest.clone()).send().await },
            )
            .await?;
        let status = response.into_inner();
        status
            .id
            .parse()
            .map_err(|err| anyhow!("invalid run ID from server: {err}"))
    }

    pub async fn list_secrets(&self) -> Result<Vec<types::SecretMetadata>> {
        let response = self
            .send_api(|client| async move { client.list_secrets().send().await })
            .await?;
        Ok(response.into_inner().data)
    }

    pub async fn create_secret(
        &self,
        body: types::CreateSecretRequest,
    ) -> Result<types::SecretMetadata> {
        let response = self
            .send_api(
                |client| async move { client.create_secret().body(body.clone()).send().await },
            )
            .await?;
        Ok(response.into_inner())
    }

    pub async fn delete_secret_by_name(&self, name: &str) -> Result<()> {
        self.send_api(|client| async move {
            client
                .delete_secret_by_name()
                .body(types::DeleteSecretRequest {
                    name: name.to_string(),
                })
                .send()
                .await
        })
        .await?;
        Ok(())
    }

    pub async fn list_models(
        &self,
        provider: Option<&str>,
        query: Option<&str>,
    ) -> Result<Vec<Model>> {
        let mut offset = 0u64;
        let mut models = Vec::new();

        loop {
            let response = self
                .send_api(|client| async move {
                    let mut request = client.list_models().page_limit(100u64).page_offset(offset);
                    if let Some(provider) = provider {
                        request = request.provider(provider.to_string());
                    }
                    if let Some(query) = query {
                        request = request.query(query.to_string());
                    }
                    request.send().await
                })
                .await?;
            let parsed = response.into_inner();
            let count = parsed.data.len() as u64;
            models.extend(convert_type::<_, Vec<Model>>(parsed.data)?);
            if !parsed.meta.has_more {
                break;
            }
            offset += count;
        }

        Ok(models)
    }

    pub async fn test_model(
        &self,
        id: &str,
        mode: Option<types::ModelTestMode>,
    ) -> Result<types::ModelTestResult> {
        let response = self
            .send_api(|client| async move {
                let mut request = client.test_model().id(id.to_string());
                if let Some(mode) = mode {
                    request = request.mode(mode);
                }
                request.send().await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn attach_events(&self, run_ids: &[String]) -> Result<progenitor_client::ByteStream> {
        let response = self
            .send_api(|client| async move {
                let mut request = client.attach_events();
                if !run_ids.is_empty() {
                    request = request.run_id(run_ids.join(","));
                }
                request.send().await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn get_system_info(&self) -> Result<types::SystemInfoResponse> {
        let response = self
            .send_api(|client| async move { client.get_system_info().send().await })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn get_system_disk_usage(&self, verbose: bool) -> Result<types::DiskUsageResponse> {
        let response = self
            .send_api(|client| async move {
                client.get_system_disk_usage().verbose(verbose).send().await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn prune_runs(
        &self,
        body: types::PruneRunsRequest,
    ) -> Result<types::PruneRunsResponse> {
        let response = self
            .send_api(|client| async move { client.prune_runs().body(body.clone()).send().await })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn get_health(&self) -> Result<()> {
        self.send_api(|client| async move { client.get_health().send().await })
            .await?;
        Ok(())
    }

    pub async fn run_diagnostics(&self) -> Result<types::DiagnosticsReport> {
        let response = self
            .send_api(|client| async move { client.run_diagnostics().send().await })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn get_github_repo(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<types::RepoCheckResponse> {
        let response = self
            .send_api(|client| async move {
                client
                    .get_github_repo()
                    .owner(owner.to_string())
                    .name(name.to_string())
                    .send()
                    .await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn run_preflight(
        &self,
        manifest: types::RunManifest,
    ) -> Result<types::PreflightResponse> {
        self.send_api(
            |client| async move { client.run_preflight().body(manifest.clone()).send().await },
        )
        .await
        .map(progenitor_client::ResponseValue::into_inner)
    }

    pub async fn render_workflow_graph(
        &self,
        request: types::RenderWorkflowGraphRequest,
    ) -> Result<Vec<u8>> {
        let response = self
            .send_api(|client| async move {
                client
                    .render_workflow_graph()
                    .body(request.clone())
                    .send()
                    .await
            })
            .await?;
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub async fn start_run(&self, run_id: &RunId, resume: bool) -> Result<()> {
        self.send_api(|client| async move {
            client
                .start_run()
                .id(run_id.to_string())
                .body(types::StartRunRequest { resume })
                .send()
                .await
        })
        .await?;
        Ok(())
    }

    pub async fn cancel_run(&self, run_id: &RunId) -> Result<()> {
        self.send_api(
            |client| async move { client.cancel_run().id(run_id.to_string()).send().await },
        )
        .await?;
        Ok(())
    }

    pub async fn archive_run(&self, run_id: &RunId) -> Result<()> {
        self.send_api(
            |client| async move { client.archive_run().id(run_id.to_string()).send().await },
        )
        .await?;
        Ok(())
    }

    pub async fn unarchive_run(&self, run_id: &RunId) -> Result<()> {
        self.send_api(|client| async move {
            client.unarchive_run().id(run_id.to_string()).send().await
        })
        .await?;
        Ok(())
    }

    pub async fn rewind_run(
        &self,
        run_id: &RunId,
        request: types::RewindRequest,
    ) -> Result<RewindRunResult> {
        let response = self
            .send_api(|client| async move {
                client
                    .rewind_run()
                    .id(run_id.to_string())
                    .body(request)
                    .send()
                    .await
            })
            .await?;
        let status = response.status().as_u16();
        Ok(RewindRunResult {
            status,
            response: response.into_inner(),
        })
    }

    pub async fn fork_run(
        &self,
        run_id: &RunId,
        request: types::ForkRequest,
    ) -> Result<types::ForkResponse> {
        let response = self
            .send_api(|client| async move {
                client
                    .fork_run()
                    .id(run_id.to_string())
                    .body(request)
                    .send()
                    .await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn run_timeline(&self, run_id: &RunId) -> Result<Vec<types::TimelineEntryResponse>> {
        let response = self
            .send_api(|client| async move {
                client
                    .get_run_timeline()
                    .id(run_id.to_string())
                    .send()
                    .await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn list_store_runs(&self) -> Result<Vec<RunSummary>> {
        let mut all_runs = Vec::new();
        let mut offset = 0_u64;
        let limit = 100_u64;

        loop {
            let response = self
                .send_api(|client| async move {
                    client
                        .list_runs()
                        .page_limit(limit)
                        .page_offset(offset)
                        .include_archived(true)
                        .send()
                        .await
                })
                .await?;
            let parsed = response.into_inner();
            let batch = parsed
                .data
                .into_iter()
                .map(convert_type)
                .collect::<Result<Vec<_>>>()?;
            let batch_len = batch.len() as u64;
            all_runs.extend(batch);

            if !parsed.meta.has_more || batch_len == 0 {
                break;
            }
            offset += batch_len;
        }

        Ok(all_runs)
    }

    pub async fn retrieve_run(&self, run_id: &RunId) -> Result<RunSummary> {
        let response = self
            .send_api(
                |client| async move { client.retrieve_run().id(run_id.to_string()).send().await },
            )
            .await?;
        convert_type(response.into_inner())
    }

    pub async fn resolve_run(&self, selector: &str) -> Result<RunSummary> {
        let response = self
            .send_api(|client| async move {
                client
                    .resolve_run()
                    .selector(selector.to_string())
                    .send()
                    .await
            })
            .await?;
        convert_type(response.into_inner())
    }

    pub async fn get_run_state(&self, run_id: &RunId) -> Result<RunProjection> {
        let response = self
            .send_api(
                |client| async move { client.get_run_state().id(run_id.to_string()).send().await },
            )
            .await?;
        convert_type(response.into_inner())
    }

    #[expect(
        clippy::disallowed_types,
        reason = "Client builds raw server API request URLs for wire transit; logging redaction is handled at log boundaries."
    )]
    pub async fn get_run_logs(&self, run_id: &RunId) -> Result<Option<String>> {
        let base_url = self.base_url();
        let mut url = fabro_http::Url::parse(&base_url)
            .with_context(|| format!("invalid server base URL {base_url}"))?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("server base URL cannot accept path segments"))?
            .extend(["api", "v1", "runs", &run_id.to_string(), "logs"]);
        let request_url = url.clone();

        let response = self
            .send_http_response(move |client| {
                let url = request_url.clone();
                async move { client.get(url).send().await }
            })
            .await?;
        match response {
            Ok(response) => {
                let bytes = response
                    .bytes()
                    .await
                    .context("failed to read run logs response body")?;
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            }
            Err(failure) if failure.status == fabro_http::StatusCode::NOT_FOUND => Ok(None),
            Err(failure) => Err(raw_response_failure_error(&failure)),
        }
    }

    pub async fn create_run_pull_request(
        &self,
        run_id: &RunId,
        force: bool,
        model: Option<String>,
    ) -> Result<fabro_types::PullRequestRecord> {
        let body = types::CreateRunPullRequestRequest { force, model };
        let response = self
            .send_api(|client| async move {
                client
                    .create_run_pull_request()
                    .id(run_id.to_string())
                    .body(body.clone())
                    .send()
                    .await
            })
            .await
            .map_err(add_pr_upgrade_hint)?;
        convert_type(response.into_inner())
    }

    pub async fn get_run_pull_request(
        &self,
        run_id: &RunId,
    ) -> Result<fabro_types::PullRequestDetail> {
        let response = self
            .send_api(|client| async move {
                client
                    .get_run_pull_request()
                    .id(run_id.to_string())
                    .send()
                    .await
            })
            .await
            .map_err(add_pr_upgrade_hint)?;
        Ok(response.into_inner())
    }

    pub async fn merge_run_pull_request(
        &self,
        run_id: &RunId,
        method: MergeStrategy,
    ) -> Result<types::MergeRunPullRequestResponse> {
        let body = types::MergeRunPullRequestRequest { method };
        let response = self
            .send_api(|client| async move {
                client
                    .merge_run_pull_request()
                    .id(run_id.to_string())
                    .body(body.clone())
                    .send()
                    .await
            })
            .await
            .map_err(add_pr_upgrade_hint)?;
        convert_type(response.into_inner())
    }

    pub async fn close_run_pull_request(
        &self,
        run_id: &RunId,
    ) -> Result<types::CloseRunPullRequestResponse> {
        let response = self
            .send_api(|client| async move {
                client
                    .close_run_pull_request()
                    .id(run_id.to_string())
                    .send()
                    .await
            })
            .await
            .map_err(add_pr_upgrade_hint)?;
        convert_type(response.into_inner())
    }

    pub async fn list_run_events(
        &self,
        run_id: &RunId,
        since_seq: Option<u32>,
        limit: Option<usize>,
    ) -> Result<Vec<EventEnvelope>> {
        let mut next_since_seq = since_seq;
        let mut all_events = Vec::new();

        loop {
            let response = self
                .send_api(|client| async move {
                    let mut request = client.list_run_events().id(run_id.to_string());
                    if let Some(seq) = next_since_seq.and_then(non_zero_u64_from_u32) {
                        request = request.since_seq(seq);
                    }
                    if let Some(limit) = limit.and_then(non_zero_u64_from_usize) {
                        request = request.limit(limit);
                    }
                    request.send().await
                })
                .await?;
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

    pub async fn attach_run_events(
        &self,
        run_id: &RunId,
        since_seq: Option<u32>,
    ) -> Result<RunEventStream> {
        let response = self
            .send_api(|client| async move {
                let mut request = client.attach_run_events().id(run_id.to_string());
                if let Some(seq) = since_seq.and_then(non_zero_u64_from_u32) {
                    request = request.since_seq(seq);
                }
                request.send().await
            })
            .await?;
        Ok(RunEventStream::new(response.into_inner()))
    }

    pub async fn list_run_questions(&self, run_id: &RunId) -> Result<Vec<types::ApiQuestion>> {
        let response = self
            .send_api(|client| async move {
                client
                    .list_run_questions()
                    .id(run_id.to_string())
                    .page_limit(100)
                    .page_offset(0)
                    .send()
                    .await
            })
            .await?;
        Ok(response.into_inner().data)
    }

    pub async fn submit_run_answer(
        &self,
        run_id: &RunId,
        qid: &str,
        value: Option<String>,
        selected_option_key: Option<String>,
        selected_option_keys: Vec<String>,
    ) -> Result<()> {
        self.send_api(|client| async move {
            client
                .submit_run_answer()
                .id(run_id.to_string())
                .qid(qid)
                .body(types::SubmitAnswerRequest {
                    value:                value.clone(),
                    selected_option_key:  selected_option_key.clone(),
                    selected_option_keys: selected_option_keys.clone(),
                })
                .send()
                .await
        })
        .await?;
        Ok(())
    }

    pub async fn append_run_event(&self, run_id: &RunId, event: &RunEvent) -> Result<u32> {
        let body: types::RunEvent = convert_type(event)?;
        let response = self
            .send_api(|client| async move {
                client
                    .append_run_event()
                    .id(run_id.to_string())
                    .body(body.clone())
                    .send()
                    .await
            })
            .await?;
        u32::try_from(response.into_inner().seq).context("append_run_event returned invalid seq")
    }

    pub async fn write_run_blob(&self, run_id: &RunId, data: &[u8]) -> Result<RunBlobId> {
        let response = self
            .send_api(|client| async move {
                client
                    .write_run_blob()
                    .id(run_id.to_string())
                    .body(data.to_vec())
                    .send()
                    .await
            })
            .await?;
        response
            .into_inner()
            .id
            .parse()
            .context("write_run_blob returned invalid blob id")
    }

    pub async fn read_run_blob(
        &self,
        run_id: &RunId,
        blob_id: &RunBlobId,
    ) -> Result<Option<Bytes>> {
        let response = self
            .current_state()
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
                let err = map_api_error(err);
                if is_not_found_error(&err) {
                    Ok(None)
                } else {
                    Err(err)
                }
            }
        }
    }

    #[expect(
        clippy::disallowed_types,
        reason = "Client builds raw server API request URLs for wire transit; logging redaction is handled at log boundaries."
    )]
    pub async fn delete_store_run(&self, run_id: &RunId, force: bool) -> Result<()> {
        let base_url = self.base_url();
        let mut url = fabro_http::Url::parse(&base_url)
            .with_context(|| format!("invalid server base URL {base_url}"))?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("server base URL cannot accept path segments"))?
            .extend(["api", "v1", "runs", &run_id.to_string()]);
        if force {
            url.query_pairs_mut().append_pair("force", "true");
        }

        self.send_http(|http_client| async move { http_client.delete(url.clone()).send().await })
            .await?;
        Ok(())
    }

    pub async fn list_run_artifacts(&self, run_id: &RunId) -> Result<Vec<types::RunArtifactEntry>> {
        let response = self
            .send_api(|client| async move {
                client
                    .list_run_artifacts()
                    .id(run_id.to_string())
                    .send()
                    .await
            })
            .await?;
        Ok(response.into_inner().data)
    }

    pub async fn download_stage_artifact(
        &self,
        run_id: &RunId,
        stage_id: &StageId,
        filename: &str,
    ) -> Result<Vec<u8>> {
        let response = self
            .send_api(|client| async move {
                client
                    .get_stage_artifact()
                    .id(run_id.to_string())
                    .stage_id(stage_id.to_string())
                    .filename(filename)
                    .send()
                    .await
            })
            .await?;
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    #[expect(
        clippy::disallowed_types,
        reason = "Client builds raw server API request URLs for wire transit; logging redaction is handled at log boundaries."
    )]
    fn stage_artifacts_url(&self, run_id: &RunId, stage_id: &StageId) -> Result<fabro_http::Url> {
        let base_url = self.base_url();
        let mut url = fabro_http::Url::parse(&base_url)
            .with_context(|| format!("invalid server base URL {base_url}"))?;
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

    pub async fn upload_stage_artifact_file(
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
            .current_state()
            .http_client
            .post(url)
            .bearer_auth(bearer_token)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, content_length.to_string())
            .body(body)
            .send()
            .await
            .with_context(|| format!("failed to upload artifact {}", path.display()))?;
        classify_http_response(response)
            .await?
            .map(|_| ())
            .map_err(|failure| raw_response_failure_error(&failure))
    }

    pub async fn upload_stage_artifact_batch(
        &self,
        run_id: &RunId,
        stage_id: &StageId,
        artifact_capture_dir: &Path,
        artifacts: &[ArtifactUpload],
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
                part:           part_name.clone(),
                path:           artifact.path.clone(),
                sha256:         Some(artifact.content_sha256.clone()),
                expected_bytes: Some(artifact.bytes),
                content_type:   Some(artifact.mime.clone()),
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
            .current_state()
            .http_client
            .post(url)
            .bearer_auth(bearer_token)
            .multipart(form)
            .send()
            .await
            .context("failed to upload artifact batch")?;
        classify_http_response(response)
            .await?
            .map(|_| ())
            .map_err(|failure| raw_response_failure_error(&failure))
    }

    pub async fn generate_preview_url(
        &self,
        run_id: &RunId,
        port: u16,
        expires_in_secs: u64,
        signed: bool,
    ) -> Result<types::PreviewUrlResponse> {
        let expires_in_secs = NonZeroU64::new(expires_in_secs)
            .ok_or_else(|| anyhow!("preview expiry must be greater than zero"))?;
        let response = self
            .send_api(|client| async move {
                client
                    .generate_preview_url()
                    .id(run_id.to_string())
                    .body(types::PreviewUrlRequest {
                        expires_in_secs,
                        port: i64::from(port),
                        signed,
                    })
                    .send()
                    .await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn create_run_ssh_access(
        &self,
        run_id: &RunId,
        ttl_minutes: f64,
    ) -> Result<types::SshAccessResponse> {
        let response = self
            .send_api(|client| async move {
                client
                    .create_run_ssh_access()
                    .id(run_id.to_string())
                    .body(types::SshAccessRequest { ttl_minutes })
                    .send()
                    .await
            })
            .await?;
        Ok(response.into_inner())
    }

    pub async fn list_sandbox_files(
        &self,
        run_id: &RunId,
        path: &str,
        depth: Option<u32>,
    ) -> Result<Vec<types::SandboxFileEntry>> {
        let response = self
            .send_api(|client| async move {
                let mut request = client
                    .list_sandbox_files()
                    .id(run_id.to_string())
                    .path(path);
                if let Some(depth) = depth.and_then(non_zero_u64_from_u32) {
                    request = request.depth(depth);
                }
                request.send().await
            })
            .await?;
        Ok(response.into_inner().data)
    }

    pub async fn get_sandbox_file(&self, run_id: &RunId, path: &str) -> Result<Vec<u8>> {
        let response = self
            .send_api(|client| async move {
                client
                    .get_sandbox_file()
                    .id(run_id.to_string())
                    .path(path)
                    .send()
                    .await
            })
            .await?;
        let mut stream = response.into_inner();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| anyhow!("{err}"))?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub async fn put_sandbox_file(&self, run_id: &RunId, path: &str, bytes: Vec<u8>) -> Result<()> {
        self.send_api(|client| async move {
            client
                .put_sandbox_file()
                .id(run_id.to_string())
                .path(path)
                .body(bytes.clone())
                .send()
                .await
        })
        .await?;
        Ok(())
    }
}

fn client_state(
    base_url: String,
    http_client: fabro_http::HttpClient,
    bearer_token: Option<String>,
) -> ClientState {
    let client = fabro_api::ApiClient::new_with_client(&base_url, http_client.clone());
    ClientState {
        client,
        http_client,
        bearer_token,
        base_url,
    }
}

fn default_transport_connector(target: ServerTarget) -> TransportConnector {
    TransportConnector::new(move |bearer_token| {
        let target = target.clone();
        async move { connect_target_transport(&target, bearer_token.as_deref()) }
    })
}

fn connect_target_transport(
    target: &ServerTarget,
    bearer_token: Option<&str>,
) -> Result<(fabro_http::HttpClient, String)> {
    if let Some(api_url) = target.as_http_url() {
        let mut builder = fabro_http::HttpClientBuilder::new();
        builder = match bearer_token {
            Some(token) => apply_bearer_token_auth(builder, token)?,
            None => builder,
        };
        let http_client = builder.build()?;
        return Ok((http_client, api_url.to_string()));
    }

    let Some(path) = target.as_unix_socket_path() else {
        bail!("server target must be an http(s) URL or absolute Unix socket path");
    };
    let mut builder = fabro_http::HttpClientBuilder::new()
        .unix_socket(path)
        .no_proxy();
    builder = match bearer_token {
        Some(token) => apply_bearer_token_auth(builder, token)?,
        None => builder,
    };
    let http_client = builder.build()?;
    Ok((http_client, "http://fabro".to_string()))
}

pub fn apply_bearer_token_auth(
    builder: fabro_http::HttpClientBuilder,
    token: &str,
) -> Result<fabro_http::HttpClientBuilder> {
    let mut headers = fabro_http::HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        fabro_http::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid bearer token header value")?,
    );
    Ok(builder.default_headers(headers))
}

fn non_zero_u64_from_u32(value: u32) -> Option<NonZeroU64> {
    NonZeroU64::new(u64::from(value))
}

fn non_zero_u64_from_usize(value: usize) -> Option<NonZeroU64> {
    u64::try_from(value).ok().and_then(NonZeroU64::new)
}

// A 404 without a structured error code means the server didn't know the
// route — PR commands moved server-side in a recent release. Point users at
// an upgrade rather than leaving them with an opaque message. A 404 with a
// code (e.g. no_stored_record) is a normal app-level response and passes
// through unchanged.
fn add_pr_upgrade_hint(err: anyhow::Error) -> anyhow::Error {
    let is_missing_route = api_failure_for(&err).is_some_and(|failure| {
        failure.status == fabro_http::StatusCode::NOT_FOUND && failure.code.is_none()
    });
    if is_missing_route {
        anyhow!(
            "{err}\n\n\
             The fabro server may not support pull request endpoints — `fabro pr` commands \
             moved server-side in a recent release. Upgrade the fabro server."
        )
    } else {
        err
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Duration as ChronoDuration;
    use fabro_util::exit;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::error::tag_with_failure;
    use crate::{AuthStore, DevTokenEntry};

    fn oauth_entry(login: &str) -> OAuthEntry {
        let now = chrono::Utc::now();
        OAuthEntry {
            access_token:             format!("access-{login}"),
            access_token_expires_at:  now + ChronoDuration::minutes(10),
            refresh_token:            format!("refresh-{login}"),
            refresh_token_expires_at: now + ChronoDuration::days(30),
            subject:                  StoredSubject {
                idp_issuer:  "https://github.com".to_string(),
                idp_subject: "12345".to_string(),
                login:       login.to_string(),
                name:        format!("Name {login}"),
                email:       format!("{login}@example.com"),
            },
            logged_in_at:             now,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refresh_access_token_allows_plain_http_targets() {
        let temp = tempfile::tempdir().unwrap();
        let auth_store = AuthStore::new(temp.path().join("auth.json"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request.starts_with("POST /auth/cli/refresh HTTP/1.1"),
                "unexpected refresh request: {request}"
            );
            let body = serde_json::json!({
                "access_token": "access-refreshed",
                "access_token_expires_at": (chrono::Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
                "refresh_token": "refresh-refreshed",
                "refresh_token_expires_at": (chrono::Utc::now() + ChronoDuration::days(30)).to_rfc3339(),
                "subject": {
                    "idp_issuer": "https://github.com",
                    "idp_subject": "12345",
                    "login": "octocat",
                    "name": "Name octocat",
                    "email": "octocat@example.com"
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let target = ServerTarget::http_url(format!("http://localhost:{port}")).unwrap();
        let entry = oauth_entry("octocat");
        auth_store
            .put(&target, AuthEntry::OAuth(entry.clone()))
            .unwrap();

        let client = Client::builder()
            .target(target.clone())
            .credential(Credential::OAuth(entry))
            .oauth_session(OAuthSession::new(target.clone(), auth_store.clone()))
            .transport("http://localhost", fabro_http::test_http_client().unwrap())
            .connect()
            .await
            .unwrap();

        client.refresh_access_token("access-octocat").await.unwrap();
        let refreshed = auth_store.get(&target).unwrap().unwrap();
        let AuthEntry::OAuth(refreshed) = refreshed else {
            panic!("expected OAuth entry");
        };
        assert_eq!(refreshed.access_token, "access-refreshed");
        assert_eq!(refreshed.refresh_token, "refresh-refreshed");
        server.abort();
    }

    async fn oauth_client(
        server: &MockServer,
    ) -> (tempfile::TempDir, Client, AuthStore, ServerTarget) {
        let temp = tempfile::tempdir().unwrap();
        let auth_store = AuthStore::new(temp.path().join("auth.json"));
        let target = ServerTarget::http_url(server.base_url()).unwrap();
        let entry = oauth_entry("octocat");
        auth_store
            .put(&target, AuthEntry::OAuth(entry.clone()))
            .unwrap();

        let client = Client::builder()
            .target(target.clone())
            .credential(Credential::OAuth(entry))
            .oauth_session(OAuthSession::new(target.clone(), auth_store.clone()))
            .transport(
                server.base_url(),
                fabro_http::HttpClientBuilder::new()
                    .no_proxy()
                    .build()
                    .unwrap(),
            )
            .connect()
            .await
            .unwrap();

        (temp, client, auth_store, target)
    }

    #[tokio::test]
    async fn refresh_access_token_reinstalls_stored_dev_token_without_refresh_request() {
        let server = MockServer::start();
        let refresh_mock = server.mock(|when, then| {
            when.method(POST).path("/auth/cli/refresh");
            then.status(500);
        });
        let temp = tempfile::tempdir().unwrap();
        let auth_store = AuthStore::new(temp.path().join("auth.json"));
        let target = ServerTarget::http_url(server.base_url()).unwrap();
        let old_token =
            "fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        let stored_token =
            "fabro_dev_abababababababababababababababababababababababababababababababab";
        auth_store
            .put(
                &target,
                AuthEntry::DevToken(DevTokenEntry {
                    token:        stored_token.to_string(),
                    logged_in_at: chrono::Utc::now(),
                }),
            )
            .unwrap();

        let seen_tokens = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_tokens_for_connector = Arc::clone(&seen_tokens);
        let base_url = server.base_url();
        let client = Client::builder()
            .target(target.clone())
            .credential(Credential::DevToken(old_token.to_string()))
            .oauth_session(OAuthSession::new(target.clone(), auth_store.clone()))
            .transport_connector(TransportConnector::new(move |bearer_token| {
                let seen_tokens = Arc::clone(&seen_tokens_for_connector);
                let base_url = base_url.clone();
                async move {
                    seen_tokens.lock().unwrap().push(bearer_token);
                    Ok((fabro_http::test_http_client().unwrap(), base_url))
                }
            }))
            .connect()
            .await
            .unwrap();

        client.refresh_access_token(old_token).await.unwrap();

        assert_eq!(refresh_mock.calls(), 0);
        assert_eq!(*seen_tokens.lock().unwrap(), vec![
            Some(old_token.to_string()),
            Some(stored_token.to_string()),
        ]);
    }

    #[tokio::test]
    async fn refresh_access_token_classifies_expired_refresh_tokens() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/auth/cli/refresh")
                .header("authorization", "Bearer refresh-octocat");
            then.status(401)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "error": "refresh_token_expired",
                    "error_description": "CLI session has expired. Run `fabro auth login` again."
                }));
        });

        let (_temp, client, auth_store, target) = oauth_client(&server).await;
        let err = client
            .refresh_access_token("access-octocat")
            .await
            .unwrap_err();

        assert_eq!(exit::exit_code_for(&err), 4);
        assert!(auth_store.get(&target).unwrap().is_none());
    }

    #[tokio::test]
    async fn refresh_access_token_keeps_server_errors_as_exit_1() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/auth/cli/refresh")
                .header("authorization", "Bearer refresh-octocat");
            then.status(500)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "error": "server_error",
                    "error_description": "OAuth server exploded."
                }));
        });

        let (_temp, client, auth_store, target) = oauth_client(&server).await;
        let err = client
            .refresh_access_token("access-octocat")
            .await
            .unwrap_err();

        assert_eq!(exit::exit_code_for(&err), 1);
        assert!(auth_store.get(&target).unwrap().is_some());
    }

    #[tokio::test]
    async fn refresh_access_token_keeps_login_not_permitted_as_exit_1() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/auth/cli/refresh")
                .header("authorization", "Bearer refresh-octocat");
            then.status(403)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "error": "unauthorized",
                    "error_description": "Login not permitted for this user."
                }));
        });

        let (_temp, client, auth_store, target) = oauth_client(&server).await;
        let err = client
            .refresh_access_token("access-octocat")
            .await
            .unwrap_err();

        assert_eq!(exit::exit_code_for(&err), 1);
        assert!(auth_store.get(&target).unwrap().is_some());
    }

    #[test]
    fn add_pr_upgrade_hint_appends_on_unstructured_404() {
        let err = tag_with_failure(
            anyhow!("request failed with status 404 Not Found"),
            ApiFailure {
                status: fabro_http::StatusCode::NOT_FOUND,
                code:   None,
            },
        );
        let wrapped = super::add_pr_upgrade_hint(err);
        let message = wrapped.to_string();
        assert!(
            message.contains("Upgrade the fabro server"),
            "expected upgrade hint, got: {message}"
        );
        assert!(message.contains("status 404"), "original preserved");
    }

    #[test]
    fn add_pr_upgrade_hint_does_not_touch_structured_404() {
        let err = tag_with_failure(
            anyhow!("No pull request found in store. Create one first with: fabro pr create abc"),
            ApiFailure {
                status: fabro_http::StatusCode::NOT_FOUND,
                code:   Some("no_stored_record".to_string()),
            },
        );
        let wrapped = super::add_pr_upgrade_hint(err);
        let message = wrapped.to_string();
        assert!(
            !message.contains("Upgrade the fabro server"),
            "hint should only fire on unstructured 404s, got: {message}"
        );
    }
}
