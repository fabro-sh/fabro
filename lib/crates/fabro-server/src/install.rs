use std::collections::HashMap;
use std::convert::Infallible;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::{OriginalUri, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router, middleware};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use fabro_auth::{AuthCredential, AuthDetails, credential_id_for};
use fabro_config::Storage;
use fabro_config::bind::{Bind, BindRequest};
use fabro_config::envfile::EnvFileUpdate;
use fabro_install::{
    InstallListenConfig, OBJECT_STORE_ACCESS_KEY_ID_ENV, OBJECT_STORE_SECRET_ACCESS_KEY_ENV,
    PendingSettingsWrite, VaultSecretWrite, merge_server_settings, persist_install_outputs_direct,
    write_github_app_settings, write_object_store_settings, write_token_settings,
};
use fabro_model::Provider;
use fabro_static::EnvVars;
use fabro_store::ArtifactStore;
use fabro_types::ServerSettings;
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::server::ObjectStoreSettings;
use fabro_util::version::FABRO_VERSION;
use fabro_util::{Home, dev_token, session_secret};
use fabro_vault::SecretType as VaultSecretType;
use object_store::aws::resolve_bucket_region;
use object_store::path::Path as ObjectStorePath;
use object_store::{ClientOptions, RetryConfig};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};
use tower::service_fn;
use tracing::{error, info, warn};
use zeroize::Zeroizing;

use crate::error::ApiError;
use crate::serve::{self, DEFAULT_TCP_PORT};
use crate::server_secrets::{ServerSecrets, process_env_snapshot};
use crate::{security_headers, static_files};

#[derive(Clone)]
pub struct InstallAppState {
    install_token:      Arc<str>,
    pending_install:    Arc<Mutex<PendingInstall>>,
    storage_dir:        Arc<Path>,
    config_path:        Arc<Path>,
    home:               Option<Home>,
    install_listen:     Arc<Mutex<InstallListenConfig>>,
    first_operator:     Arc<Mutex<Option<InstallOperatorFingerprint>>>,
    finish_in_progress: Arc<AtomicBool>,
    upstreams:          InstallUpstreamConfig,
    on_finish:          Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone, Debug, Default)]
struct InstallUpstreamConfig {
    provider_base_urls:  HashMap<Provider, String>,
    github_api_base_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstallOperatorFingerprint {
    user_agent: Option<String>,
    remote_ip:  Option<String>,
}

pub const DEFAULT_INSTALL_GITHUB_API_BASE_URL: &str = "https://api.github.com";
const DEFAULT_INSTALL_TCP_LISTEN_ADDRESS: &str = "127.0.0.1:32276";
const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const REDACTED_SECRET_VALUE: &str = "[REDACTED]";
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(20);
const VALIDATION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

impl InstallAppState {
    #[must_use]
    pub fn new(token: String, storage_dir: &Path, config_path: &Path) -> Self {
        Self {
            install_token:      Arc::from(token),
            pending_install:    Arc::new(Mutex::new(PendingInstall::default())),
            storage_dir:        Arc::from(storage_dir),
            config_path:        Arc::from(config_path),
            home:               None,
            install_listen:     Arc::new(Mutex::new(InstallListenConfig::Tcp(
                DEFAULT_INSTALL_TCP_LISTEN_ADDRESS.to_string(),
            ))),
            first_operator:     Arc::new(Mutex::new(None)),
            finish_in_progress: Arc::new(AtomicBool::new(false)),
            upstreams:          InstallUpstreamConfig::default(),
            on_finish:          None,
        }
    }

    #[must_use]
    pub fn for_test(token: &str) -> Self {
        let temp_root = std::env::temp_dir().join("fabro-install-test");
        Self::for_test_with_paths(token, &temp_root, &temp_root.join("settings.toml"))
    }

    #[must_use]
    #[expect(
        unsafe_code,
        reason = "test-only: set FABRO_TEST_IN_MEMORY_STORE to a constant so install tests \
                  don't hang on real S3; parallel tests race on the same value"
    )]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-only: forces the in-memory object store for install tests so they \
                  don't contact real S3"
    )]
    pub fn for_test_with_paths(token: &str, storage_dir: &Path, config_path: &Path) -> Self {
        // Install-flow tests verify persistence and redaction, not S3
        // reachability. Force the in-memory object store shortcut so
        // /install/finish can't hang on an unreachable bucket.
        unsafe {
            std::env::set_var(EnvVars::FABRO_TEST_IN_MEMORY_STORE, "1");
        }
        Self {
            install_token:      Arc::from(token),
            pending_install:    Arc::new(Mutex::new(PendingInstall::default())),
            storage_dir:        Arc::from(storage_dir),
            config_path:        Arc::from(config_path),
            home:               None,
            install_listen:     Arc::new(Mutex::new(InstallListenConfig::Tcp(
                DEFAULT_INSTALL_TCP_LISTEN_ADDRESS.to_string(),
            ))),
            first_operator:     Arc::new(Mutex::new(None)),
            finish_in_progress: Arc::new(AtomicBool::new(false)),
            upstreams:          InstallUpstreamConfig::default(),
            on_finish:          None,
        }
    }

    #[must_use]
    pub fn with_finish_callback(self, on_finish: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            on_finish: Some(on_finish),
            ..self
        }
    }

    #[must_use]
    pub fn with_home(mut self, home: Home) -> Self {
        self.home = Some(home);
        self
    }

    #[must_use]
    pub fn with_provider_base_url(
        mut self,
        provider: Provider,
        base_url: impl Into<String>,
    ) -> Self {
        self.upstreams
            .provider_base_urls
            .insert(provider, base_url.into());
        self
    }

    #[must_use]
    pub fn with_github_api_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.upstreams.github_api_base_url = Some(base_url.into());
        self
    }

    fn set_install_bind(&self, bind: &Bind) {
        *lock_unpoisoned(&self.install_listen, "install listen") = install_listen_config(bind);
    }

    fn install_listen_config(&self) -> InstallListenConfig {
        lock_unpoisoned(&self.install_listen, "install listen").clone()
    }
}

#[derive(Deserialize, Default)]
struct InstallTokenQuery {
    token: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct PendingInstall {
    llm:                Option<LlmProvidersInput>,
    server:             Option<ServerConfigInput>,
    object_store:       Option<InstallObjectStoreState>,
    github:             Option<GithubInstallState>,
    pending_github_app: Option<PendingGithubApp>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LlmProvidersInput {
    providers: Vec<LlmProviderInput>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LlmProviderInput {
    provider: Provider,
    api_key:  String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ServerConfigInput {
    canonical_url: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, strum::IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
enum InstallObjectStoreProvider {
    Local,
    S3,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, strum::IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
enum InstallObjectStoreCredentialMode {
    Runtime,
    AccessKey,
}

#[derive(Clone, Debug, Deserialize)]
struct InstallObjectStoreInput {
    provider:          InstallObjectStoreProvider,
    bucket:            Option<String>,
    region:            Option<String>,
    credential_mode:   Option<InstallObjectStoreCredentialMode>,
    access_key_id:     Option<String>,
    secret_access_key: Option<String>,
}

#[derive(Clone)]
struct InstallSecret(Zeroizing<String>);

impl InstallSecret {
    fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for InstallSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED_SECRET_VALUE)
    }
}

impl std::fmt::Display for InstallSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED_SECRET_VALUE)
    }
}

impl Serialize for InstallSecret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(REDACTED_SECRET_VALUE)
    }
}

#[derive(Clone)]
struct InstallAwsCredentialPair {
    access_key_id:     InstallSecret,
    secret_access_key: InstallSecret,
}

impl InstallAwsCredentialPair {
    fn new(access_key_id: impl Into<String>, secret_access_key: impl Into<String>) -> Self {
        Self {
            access_key_id:     InstallSecret::new(access_key_id),
            secret_access_key: InstallSecret::new(secret_access_key),
        }
    }
}

impl std::fmt::Debug for InstallAwsCredentialPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallAwsCredentialPair")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &self.secret_access_key)
            .finish()
    }
}

#[derive(Clone, Debug)]
enum InstallObjectStoreState {
    Local,
    S3 {
        bucket:             String,
        region:             String,
        credential_mode:    InstallObjectStoreCredentialMode,
        manual_credentials: Option<InstallAwsCredentialPair>,
    },
}

impl InstallObjectStoreState {
    fn as_session_value(&self) -> serde_json::Value {
        match self {
            Self::Local => serde_json::json!({
                "provider": "local",
            }),
            Self::S3 {
                bucket,
                region,
                credential_mode,
                manual_credentials,
            } => serde_json::json!({
                "provider": "s3",
                "bucket": bucket,
                "region": region,
                "credential_mode": <&'static str>::from(*credential_mode),
                "manual_credentials_saved": matches!(
                    credential_mode,
                    InstallObjectStoreCredentialMode::AccessKey
                ) && manual_credentials.is_some(),
            }),
        }
    }

    fn to_persistence_selection(&self) -> fabro_install::InstallObjectStoreSelection {
        match self {
            Self::Local => fabro_install::InstallObjectStoreSelection::Local,
            Self::S3 {
                bucket,
                region,
                credential_mode,
                manual_credentials,
            } => fabro_install::InstallObjectStoreSelection::S3 {
                bucket:            bucket.clone(),
                region:            region.clone(),
                credential_mode:   match credential_mode {
                    InstallObjectStoreCredentialMode::Runtime => {
                        fabro_install::InstallObjectStoreCredentialMode::Runtime
                    }
                    InstallObjectStoreCredentialMode::AccessKey => {
                        fabro_install::InstallObjectStoreCredentialMode::AccessKey
                    }
                },
                access_key_id:     manual_credentials
                    .as_ref()
                    .map(|credentials| credentials.access_key_id.expose_secret().to_string()),
                secret_access_key: manual_credentials
                    .as_ref()
                    .map(|credentials| credentials.secret_access_key.expose_secret().to_string()),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GithubTokenInput {
    token:    String,
    username: String,
}

#[derive(Clone, Debug)]
enum GithubInstallState {
    Token(GithubTokenInput),
    App(GithubAppInstall),
}

#[derive(Clone, Debug)]
struct PendingGithubApp {
    state:            String,
    owner:            GitHubAppOwner,
    app_name:         String,
    allowed_username: String,
    expires_at:       Instant,
}

#[derive(Clone, Debug)]
struct GithubAppInstall {
    owner:            GitHubAppOwner,
    app_name:         String,
    allowed_username: String,
    app_id:           String,
    slug:             String,
    client_id:        String,
    client_secret:    String,
    webhook_secret:   Option<String>,
    pem:              String,
}

#[derive(Clone, Debug, Deserialize)]
struct InstallLlmTestInput {
    provider: Provider,
    api_key:  String,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubTokenTestInput {
    token: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAppManifestInput {
    owner:            GithubAppOwnerInput,
    app_name:         String,
    allowed_username: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAppOwnerInput {
    kind: GithubAppOwnerKind,
    slug: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum GithubAppOwnerKind {
    Personal,
    Org,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAppRedirectQuery {
    code:  Option<String>,
    state: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubUserResponse {
    login: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GitHubAppManifestConversion {
    id:             i64,
    slug:           String,
    client_id:      String,
    client_secret:  String,
    webhook_secret: Option<String>,
    pem:            String,
}

#[derive(Clone, Debug)]
enum GitHubAppOwner {
    Personal,
    Organization(String),
}

impl GitHubAppOwner {
    fn manifest_form_action(&self) -> String {
        match self {
            Self::Personal => "https://github.com/settings/apps/new".to_string(),
            Self::Organization(org) => {
                format!("https://github.com/organizations/{org}/settings/apps/new")
            }
        }
    }

    fn as_session_value(&self) -> serde_json::Value {
        match self {
            Self::Personal => serde_json::json!({ "kind": "personal" }),
            Self::Organization(org) => serde_json::json!({ "kind": "org", "slug": org }),
        }
    }
}

impl TryFrom<GithubAppOwnerInput> for GitHubAppOwner {
    type Error = String;

    fn try_from(value: GithubAppOwnerInput) -> Result<Self, Self::Error> {
        match value.kind {
            GithubAppOwnerKind::Personal => Ok(Self::Personal),
            GithubAppOwnerKind::Org => {
                let slug = value.slug.unwrap_or_default();
                let trimmed = slug.trim();
                if trimmed.is_empty() {
                    return Err("organization owner requires a non-empty slug".to_string());
                }
                Ok(Self::Organization(trimmed.to_string()))
            }
        }
    }
}

pub async fn build_install_router(state: InstallAppState) -> Router {
    static_files::assert_install_mode_shell_ready().await;

    Router::new()
        .route("/health", get(health))
        .route("/install/session", get(get_install_session))
        .route("/install/llm/test", post(post_install_llm_test))
        .route(
            "/install/llm",
            get(render_install_shell).put(put_install_llm),
        )
        .route(
            "/install/server",
            get(render_install_shell).put(put_install_server),
        )
        .route(
            "/install/object-store/test",
            post(post_install_object_store_test),
        )
        .route(
            "/install/object-store",
            get(render_install_shell).put(put_install_object_store),
        )
        .route(
            "/install/github/token/test",
            post(post_install_github_token_test),
        )
        .route("/install/github/token", put(put_install_github_token))
        .route(
            "/install/github/app/manifest",
            post(post_install_github_app_manifest),
        )
        .route(
            "/install/github/app/redirect",
            get(get_install_github_app_redirect),
        )
        .route("/install/finish", post(post_install_finish))
        .with_state(state)
        .fallback_service(service_fn(move |req: Request| async move {
            let path = req.uri().path().to_string();
            if path.starts_with("/api/") {
                Ok::<_, Infallible>(StatusCode::NOT_FOUND.into_response())
            } else if matches!(req.method(), &Method::GET | &Method::HEAD) {
                let headers = req.headers().clone();
                Ok::<_, Infallible>(static_files::serve_install(&path, &headers).await)
            } else {
                Ok::<_, Infallible>(StatusCode::NOT_FOUND.into_response())
            }
        }))
        .layer(middleware::from_fn(security_headers::layer))
}

struct InstallFinishGuard {
    flag:    Arc<AtomicBool>,
    release: bool,
}

impl InstallFinishGuard {
    fn try_acquire(flag: Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Self {
            flag,
            release: true,
        })
    }

    fn disarm(mut self) {
        self.release = false;
    }
}

impl Drop for InstallFinishGuard {
    fn drop(&mut self) {
        if self.release {
            self.flag.store(false, Ordering::Release);
        }
    }
}

pub async fn serve_install_command<F>(
    bind_request: BindRequest,
    state: InstallAppState,
    on_ready: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&Bind) -> anyhow::Result<()>,
{
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let finish_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = shutdown_tx.send(true);
    });
    let bound_listener = bind_install_listener(&bind_request).await?;
    state.set_install_bind(&bound_listener.bind);
    let state = state.with_finish_callback(finish_callback);
    let router = build_install_router(state).await;
    let bind = bound_listener.bind.clone();
    on_ready(&bind)?;

    match bound_listener.listener {
        BoundInstallListener::Unix(listener) => {
            axum::serve(listener, router)
                .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()))
                .await?;
        }
        BoundInstallListener::Tcp(listener) => {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()))
            .await?;
        }
    }

    Ok(())
}

fn lock_unpoisoned<'a, T>(mutex: &'a Mutex<T>, label: &'static str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        error!(lock = label, "recovering from poisoned install lock");
        poisoned.into_inner()
    })
}

fn install_listen_config(bind: &Bind) -> InstallListenConfig {
    match bind {
        Bind::Tcp(address) => InstallListenConfig::Tcp(address.to_string()),
        Bind::Unix(path) => InstallListenConfig::Unix(path.clone()),
    }
}

async fn health() -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "mode": "install",
    }))
    .into_response()
}

async fn get_install_session(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
) -> Response {
    if !token_is_valid(&state, &headers, query.token.as_deref()) {
        return ApiError::new(StatusCode::UNAUTHORIZED, "invalid install token").into_response();
    }
    observe_operator(&state, &headers);

    let pending_install = lock_unpoisoned(&state.pending_install, "install session").clone();

    Json(serde_json::json!({
        "completed_steps": completed_steps(&pending_install),
        "llm": redacted_llm(&pending_install),
        "server": pending_install.server,
        "object_store": redacted_object_store(&pending_install),
        "github": redacted_github(&pending_install),
        "prefill": {
            "canonical_url": detect_canonical_url(&headers),
        }
    }))
    .into_response()
}

async fn post_install_llm_test(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<InstallLlmTestInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    if let Some(error) = unsupported_install_provider_error(input.provider) {
        return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, error);
    }

    if input.api_key.trim().is_empty() {
        return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, "api_key is required");
    }
    match validate_llm_provider(&state, &input).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(err) => {
            warn!(provider = %input.provider.as_str(), error = %err, "install LLM validation failed");
            install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err)
        }
    }
}

async fn put_install_llm(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<LlmProvidersInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    if input.providers.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "at least one LLM provider is required" })),
        )
            .into_response();
    }

    for provider in &input.providers {
        if let Some(error) = unsupported_install_provider_error(provider.provider) {
            return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, error);
        }
        if provider.api_key.trim().is_empty() {
            return install_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("api_key is required for {}", provider.provider.as_str()),
            );
        }
    }

    lock_unpoisoned(&state.pending_install, "install session").llm = Some(input);
    info!(step = "llm", "install step completed");
    StatusCode::NO_CONTENT.into_response()
}

fn unsupported_install_provider_error(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::OpenAiCompatible => Some("openai_compatible is not supported by install in v1"),
        _ => None,
    }
}

async fn put_install_server(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(mut input): Json<ServerConfigInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    let canonical_url = input.canonical_url.trim();
    if canonical_url.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "canonical_url is required" })),
        )
            .into_response();
    }

    if let Err(err) = validate_canonical_url(canonical_url) {
        return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err);
    }
    input.canonical_url = canonical_url.to_string();

    lock_unpoisoned(&state.pending_install, "install session").server = Some(input);
    info!(step = "server", "install step completed");
    StatusCode::NO_CONTENT.into_response()
}

async fn post_install_object_store_test(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<InstallObjectStoreInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    let selection = {
        let pending_install = lock_unpoisoned(&state.pending_install, "install session");
        match resolve_install_object_store_state(pending_install.object_store.as_ref(), input) {
            Ok(selection) => selection,
            Err(err) => return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err),
        }
    };

    match validate_install_object_store_selection(&state, &selection).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(err) => {
            warn!(error = %err, "install object store validation failed");
            install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err)
        }
    }
}

async fn put_install_object_store(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<InstallObjectStoreInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    let mut pending_install = lock_unpoisoned(&state.pending_install, "install session");
    let selection =
        match resolve_install_object_store_state(pending_install.object_store.as_ref(), input) {
            Ok(selection) => selection,
            Err(err) => return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err),
        };

    pending_install.object_store = Some(selection);
    info!(step = "object_store", "install step completed");
    StatusCode::NO_CONTENT.into_response()
}

fn trim_install_field(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_install_object_store_state(
    current: Option<&InstallObjectStoreState>,
    input: InstallObjectStoreInput,
) -> Result<InstallObjectStoreState, String> {
    let bucket = trim_install_field(input.bucket);
    let region = trim_install_field(input.region);
    let access_key_id = trim_install_field(input.access_key_id);
    let secret_access_key = trim_install_field(input.secret_access_key);

    match input.provider {
        InstallObjectStoreProvider::Local => {
            if bucket.is_some()
                || region.is_some()
                || input.credential_mode.is_some()
                || access_key_id.is_some()
                || secret_access_key.is_some()
            {
                return Err(
                    "Local disk does not accept S3 bucket, region, or AWS credential fields."
                        .to_string(),
                );
            }
            Ok(InstallObjectStoreState::Local)
        }
        InstallObjectStoreProvider::S3 => {
            let bucket = bucket.ok_or_else(|| "Bucket is required.".to_string())?;
            let region = region
                .ok_or_else(|| "Region is required. Use a value like us-east-1.".to_string())?;
            let credential_mode = input
                .credential_mode
                .ok_or_else(|| "Choose how Fabro should authenticate to AWS.".to_string())?;
            let manual_credentials = match credential_mode {
                InstallObjectStoreCredentialMode::Runtime => {
                    if access_key_id.is_some() || secret_access_key.is_some() {
                        return Err(
                            "AWS access key fields are only allowed when using manual AWS access key credentials."
                                .to_string(),
                        );
                    }
                    None
                }
                InstallObjectStoreCredentialMode::AccessKey => Some(resolve_s3_manual_credentials(
                    access_key_id,
                    secret_access_key,
                    current,
                )?),
            };

            Ok(InstallObjectStoreState::S3 {
                bucket,
                region,
                credential_mode,
                manual_credentials,
            })
        }
    }
}

fn resolve_s3_manual_credentials(
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
    current: Option<&InstallObjectStoreState>,
) -> Result<InstallAwsCredentialPair, String> {
    match (access_key_id, secret_access_key) {
        (Some(access_key_id), Some(secret_access_key)) => Ok(InstallAwsCredentialPair::new(
            access_key_id,
            secret_access_key,
        )),
        (None, None) => current
            .and_then(|state| match state {
                InstallObjectStoreState::S3 {
                    credential_mode: InstallObjectStoreCredentialMode::AccessKey,
                    manual_credentials,
                    ..
                } => manual_credentials.clone(),
                _ => None,
            })
            .ok_or_else(|| {
                "Enter both AWS access key fields or switch to runtime credentials.".to_string()
            }),
        (Some(_), None) | (None, Some(_)) => {
            Err("Enter both AWS access key fields or switch to runtime credentials.".to_string())
        }
    }
}

fn object_store_validation_settings(
    selection: &InstallObjectStoreState,
) -> Option<ObjectStoreSettings> {
    match selection {
        InstallObjectStoreState::Local => None,
        InstallObjectStoreState::S3 { bucket, region, .. } => Some(ObjectStoreSettings::S3 {
            bucket:     InterpString::parse(bucket),
            region:     InterpString::parse(region),
            endpoint:   None,
            path_style: false,
        }),
    }
}

fn install_object_store_lookup<'a>(
    server_secrets: &'a ServerSecrets,
    manual_credentials: Option<&'a InstallAwsCredentialPair>,
) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| match (manual_credentials, name) {
        (Some(credentials), OBJECT_STORE_ACCESS_KEY_ID_ENV) => {
            Some(credentials.access_key_id.expose_secret().to_string())
        }
        (Some(credentials), OBJECT_STORE_SECRET_ACCESS_KEY_ENV) => {
            Some(credentials.secret_access_key.expose_secret().to_string())
        }
        (Some(_), EnvVars::AWS_SESSION_TOKEN) => None,
        _ => server_secrets.get(name),
    }
}

async fn validate_install_object_store_selection(
    state: &InstallAppState,
    selection: &InstallObjectStoreState,
) -> Result<(), String> {
    let Some(settings) = object_store_validation_settings(selection) else {
        return Ok(());
    };

    let (bucket, region, manual_credentials) = match selection {
        InstallObjectStoreState::Local => return Ok(()),
        InstallObjectStoreState::S3 {
            bucket,
            region,
            credential_mode: _,
            manual_credentials,
        } => (
            bucket.as_str(),
            region.as_str(),
            manual_credentials.as_ref(),
        ),
    };

    let client_options = ClientOptions::new()
        .with_connect_timeout(VALIDATION_CONNECT_TIMEOUT)
        .with_timeout(VALIDATION_TIMEOUT);
    match timeout(
        VALIDATION_TIMEOUT,
        resolve_bucket_region(bucket, &client_options),
    )
    .await
    {
        Ok(Ok(actual_region)) if actual_region != region => {
            return Err(format!(
                "Bucket {bucket} is in region {actual_region}, not {region}. Use the bucket's AWS region and try again."
            ));
        }
        Ok(Err(err)) => {
            let rendered = err.to_string();
            if rendered.contains("not found") {
                return Err(format!("Bucket {bucket} was not found."));
            }
        }
        Err(_) => {
            return Err(VALIDATION_TIMEOUT_MSG.to_string());
        }
        Ok(Ok(_)) => {}
    }

    let server_env_path = Storage::new(state.storage_dir.as_ref())
        .runtime_directory()
        .env_path();
    let server_secrets = ServerSecrets::load(server_env_path, process_env_snapshot())
        .map_err(|err| err.to_string())?;
    let build_options = serve::ObjectStoreBuildOptions {
        client_options,
        retry_config: RetryConfig {
            max_retries: 0,
            retry_timeout: VALIDATION_TIMEOUT,
            ..RetryConfig::default()
        },
    };
    let env_lookup = install_object_store_lookup(&server_secrets, manual_credentials);
    let object_store = serve::build_object_store_from_settings_with_lookup(
        &settings,
        &env_lookup,
        Some(&build_options),
    )
    .map_err(|err| err.to_string())?;

    let probe_prefix = |index: usize, prefix: &'static str| {
        let object_store = &object_store;
        async move {
            let path = ObjectStorePath::from(prefix);
            object_store
                .list_with_delimiter(Some(&path))
                .await
                .map(|_| ())
                .map_err(|err| (index, err))
        }
    };
    let probe = async {
        tokio::try_join!(probe_prefix(0, "artifacts"), probe_prefix(1, "slatedb")).map(|_| ())
    };

    match timeout(VALIDATION_TIMEOUT, probe).await {
        Ok(Ok(())) => Ok(()),
        Err(_) => Err(VALIDATION_TIMEOUT_MSG.to_string()),
        Ok(Err((index, err))) => Err(classify_object_store_validation_error(
            bucket, region, index, &err,
        )),
    }
}

const PREFIX_ACCESS_ERROR_MSG: &str = "Fabro reached the bucket but could not verify access to slatedb/ and artifacts/. Validation requires bucket list access plus object access under both prefixes.";
const VALIDATION_TIMEOUT_MSG: &str = "Timed out while checking S3 access. Verify the bucket, region, and network path, then try again.";

fn bucket_credentials_error(bucket: &str, region: &str) -> String {
    format!("Could not access bucket {bucket} in region {region} with the selected credentials.")
}

fn classify_object_store_validation_error(
    bucket: &str,
    region: &str,
    prefix_index: usize,
    err: &object_store::Error,
) -> String {
    let credentials_or_prefix_error = || {
        if prefix_index == 0 {
            bucket_credentials_error(bucket, region)
        } else {
            PREFIX_ACCESS_ERROR_MSG.to_string()
        }
    };
    match err {
        object_store::Error::PermissionDenied { .. }
        | object_store::Error::Unauthenticated { .. } => credentials_or_prefix_error(),
        object_store::Error::NotFound { .. } => format!("Bucket {bucket} was not found."),
        object_store::Error::Generic { .. } => {
            let rendered = err.to_string();
            if rendered.contains("incorrectly configured region") {
                format!(
                    "Bucket {bucket} is not reachable in region {region}. Verify the AWS region and try again."
                )
            } else if rendered.contains("not found") {
                format!("Bucket {bucket} was not found.")
            } else {
                credentials_or_prefix_error()
            }
        }
        _ => PREFIX_ACCESS_ERROR_MSG.to_string(),
    }
}

async fn post_install_github_token_test(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<GithubTokenTestInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    if input.token.trim().is_empty() {
        return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, "token is required");
    }

    match validate_github_token(&state, input.token.trim()).await {
        Ok(username) => Json(serde_json::json!({ "username": username })).into_response(),
        Err(err) => {
            warn!(error = %err, "install GitHub token validation failed");
            install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err)
        }
    }
}

async fn put_install_github_token(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<GithubTokenInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    if input.token.trim().is_empty() || input.username.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "token and username are required" })),
        )
            .into_response();
    }

    lock_unpoisoned(&state.pending_install, "install session").github =
        Some(GithubInstallState::Token(input));
    info!(step = "github_token", "install step completed");
    StatusCode::NO_CONTENT.into_response()
}

async fn post_install_github_app_manifest(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<GithubAppManifestInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);

    let owner = match GitHubAppOwner::try_from(input.owner) {
        Ok(owner) => owner,
        Err(err) => {
            return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err);
        }
    };
    if input.app_name.trim().is_empty() {
        return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, "app_name is required");
    }
    if input.allowed_username.trim().is_empty() {
        return install_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "allowed_username is required",
        );
    }

    let mut pending_install = lock_unpoisoned(&state.pending_install, "install session");
    let Some(server) = pending_install.server.clone() else {
        return missing_step_response("server");
    };

    let state_token = generate_ephemeral_secret();
    let manifest = build_github_app_manifest(
        input.app_name.trim(),
        &format!("{}/install/github/app/redirect", server.canonical_url),
        &format!("{}/auth/callback/github", server.canonical_url),
        &format!("{}/setup", server.canonical_url),
    );

    pending_install.pending_github_app = Some(PendingGithubApp {
        state:            state_token.clone(),
        owner:            owner.clone(),
        app_name:         input.app_name.trim().to_string(),
        allowed_username: input.allowed_username.trim().to_string(),
        expires_at:       Instant::now() + Duration::from_mins(10),
    });

    Json(serde_json::json!({
        "manifest": manifest,
        "github_form_action": owner.manifest_form_action(),
        "state": state_token,
    }))
    .into_response()
}

async fn get_install_github_app_redirect(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<GithubAppRedirectQuery>,
) -> Response {
    observe_operator(&state, &headers);

    let Some(state_token) = query
        .state
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return install_github_redirect_error(&state, "missing-install-github-app-state");
    };
    let Some(code) = query
        .code
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return install_github_redirect_error(&state, "missing-install-github-app-code");
    };

    let pending = {
        let pending_install = lock_unpoisoned(&state.pending_install, "install session");
        let Some(pending) = pending_install.pending_github_app.clone() else {
            return install_github_redirect_error(&state, "missing-install-github-app-state");
        };
        if pending.expires_at <= Instant::now() {
            return install_github_redirect_error(&state, "expired-install-github-app-state");
        }
        if pending.state != state_token {
            return install_github_redirect_error(&state, "invalid-install-github-app-state");
        }
        pending
    };

    match exchange_github_app_manifest_code(&state, code).await {
        Ok(conversion) => {
            let mut pending_install = lock_unpoisoned(&state.pending_install, "install session");
            let Some(still_pending) = pending_install.pending_github_app.as_ref() else {
                return install_github_redirect_error(&state, "missing-install-github-app-state");
            };
            if still_pending.state != pending.state {
                return install_github_redirect_error(&state, "invalid-install-github-app-state");
            }
            pending_install.pending_github_app = None;
            pending_install.github = Some(GithubInstallState::App(GithubAppInstall {
                owner:            pending.owner,
                app_name:         pending.app_name,
                allowed_username: pending.allowed_username,
                app_id:           conversion.id.to_string(),
                slug:             conversion.slug,
                client_id:        conversion.client_id,
                client_secret:    conversion.client_secret,
                webhook_secret:   conversion.webhook_secret,
                pem:              conversion.pem,
            }));
            info!(step = "github_app", "install step completed");
            (StatusCode::FOUND, [(
                header::LOCATION,
                format!("/install/github/done?token={}", &*state.install_token),
            )])
                .into_response()
        }
        Err(err) => {
            error!(error = %err, "install GitHub app exchange failed");
            install_github_redirect_error(&state, "github-app-manifest-conversion-failed")
        }
    }
}

async fn post_install_finish(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }
    observe_operator(&state, &headers);
    let Some(finish_guard) = InstallFinishGuard::try_acquire(Arc::clone(&state.finish_in_progress))
    else {
        return install_error_response(StatusCode::CONFLICT, "install finish already in progress");
    };

    let pending_install = lock_unpoisoned(&state.pending_install, "install session").clone();

    let Some(server) = pending_install.server else {
        return missing_step_response("server");
    };
    let Some(object_store) = pending_install.object_store else {
        return missing_step_response("object_store");
    };
    let Some(llm) = pending_install.llm else {
        return missing_step_response("llm");
    };
    let Some(github) = pending_install.github else {
        return missing_step_response("github");
    };

    let mut settings_doc = toml::Value::Table(toml::Table::default());
    let install_listen = state.install_listen_config();
    if let Err(err) =
        merge_server_settings(&mut settings_doc, &server.canonical_url, &install_listen)
    {
        return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }
    let object_store_env_plan = match write_object_store_settings(
        &mut settings_doc,
        &object_store.to_persistence_selection(),
    ) {
        Ok(plan) => plan,
        Err(err) => {
            return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };
    let mut vault_secrets = Vec::new();
    for provider in llm.providers {
        let credential = AuthCredential {
            provider: provider.provider,
            details:  AuthDetails::ApiKey {
                key: provider.api_key,
            },
        };
        let name = match credential_id_for(&credential) {
            Ok(name) => name,
            Err(err) => return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err),
        };
        let value = match serde_json::to_string(&credential) {
            Ok(value) => value,
            Err(err) => {
                return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
            }
        };
        vault_secrets.push(VaultSecretWrite {
            name,
            value,
            secret_type: VaultSecretType::Credential,
            description: None,
        });
    }

    let make_env_write = |key: &str, value: String| EnvFileUpdate {
        key: key.to_string(),
        value,
        comment: None,
    };
    let mut server_env_writes = object_store_env_plan.writes;
    let server_env_removals = object_store_env_plan.removals;
    let mut dev_token: Option<String> = None;
    match github {
        GithubInstallState::Token(github) => {
            if let Err(err) = write_token_settings(&mut settings_doc) {
                return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
            }
            vault_secrets.push(VaultSecretWrite {
                name:        EnvVars::GITHUB_TOKEN.to_string(),
                value:       github.token,
                secret_type: VaultSecretType::Environment,
                description: None,
            });
            let home = state.home.clone().unwrap_or_else(Home::from_env);
            let token = match dev_token::read_or_mint_dev_token_for_install(&home.dev_token_path())
            {
                Ok(value) => value,
                Err(err) => {
                    return install_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        err.to_string(),
                    );
                }
            };
            if let Err(err) = dev_token::write_dev_token(
                &Storage::new(state.storage_dir.as_ref())
                    .runtime_directory()
                    .dev_token_path(),
                &token,
            ) {
                return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
            }
            dev_token = Some(token);
        }
        GithubInstallState::App(github) => {
            if let Err(err) = write_github_app_settings(
                &mut settings_doc,
                &github.app_id,
                &github.slug,
                &github.client_id,
                &[github.allowed_username],
            ) {
                return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
            }
            server_env_writes.push(make_env_write(
                EnvVars::GITHUB_APP_PRIVATE_KEY,
                BASE64_STANDARD.encode(github.pem.as_bytes()),
            ));
            server_env_writes.push(make_env_write(
                EnvVars::GITHUB_APP_CLIENT_SECRET,
                github.client_secret,
            ));
            if let Some(secret) = github.webhook_secret {
                server_env_writes.push(make_env_write(EnvVars::GITHUB_APP_WEBHOOK_SECRET, secret));
            }
        }
    }

    let settings_toml = match toml::to_string_pretty(&settings_doc) {
        Ok(value) => value,
        Err(err) => {
            return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };

    let session_secret = session_secret::generate_session_secret();
    server_env_writes.push(make_env_write(EnvVars::SESSION_SECRET, session_secret));
    if let Some(token) = dev_token.as_ref() {
        server_env_writes.push(make_env_write(EnvVars::FABRO_DEV_TOKEN, token.clone()));
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "install-finish handler: reads current settings file once to produce a rollback \
                  snapshot before writing the new settings; one-shot per install-finish request"
    )]
    let previous_settings = std::fs::read_to_string(state.config_path.as_ref()).ok();

    if let Err(err) = persist_install_outputs_direct(
        state.storage_dir.as_ref(),
        &server_env_writes,
        &server_env_removals,
        &vault_secrets,
        Some(&PendingSettingsWrite {
            path:              state.config_path.as_ref(),
            contents:          &settings_toml,
            previous_contents: previous_settings.as_deref(),
        }),
    ) {
        error!(error = %err, "install persistence failed");
        let status = StatusCode::INTERNAL_SERVER_ERROR;
        let detail = err.to_string();
        let title = status.canonical_reason().unwrap_or("Unknown").to_string();
        let leftover_env_keys: Vec<String> = if err.server_env_applied {
            server_env_writes
                .iter()
                .map(|write| write.key.clone())
                .collect()
        } else {
            Vec::new()
        };
        let removed_env_keys: Vec<String> = if err.server_env_applied {
            err.removed_env_keys.clone()
        } else {
            Vec::new()
        };
        return (
            status,
            Json(serde_json::json!({
                "errors": [{
                    "status": status.as_u16().to_string(),
                    "title": title,
                    "detail": detail,
                }],
                "leftover_env_keys": leftover_env_keys,
                "removed_env_keys": removed_env_keys,
            })),
        )
            .into_response();
    }

    if let Ok(settings) = fabro_config::ServerSettingsBuilder::from_toml(&settings_toml) {
        if let Err(err) = write_artifact_store_metadata(&settings, state.storage_dir.as_ref()).await
        {
            warn!(error = %err, "failed to write artifact store metadata after install");
        }
    }
    if let Some(InstallObjectStoreState::S3 {
        credential_mode: InstallObjectStoreCredentialMode::AccessKey,
        manual_credentials,
        ..
    }) = lock_unpoisoned(&state.pending_install, "install session")
        .object_store
        .as_mut()
    {
        *manual_credentials = None;
    }

    if let Some(on_finish) = state.on_finish.clone() {
        info!(restart_url = %server.canonical_url, "install finish succeeded");
        info!("install exit scheduled");
        tokio::spawn(async move {
            sleep(Duration::from_millis(500)).await;
            on_finish();
        });
    } else {
        info!(restart_url = %server.canonical_url, "install finish succeeded");
    }
    finish_guard.disarm();

    let mut body = serde_json::json!({
        "status": "completing",
        "restart_url": server.canonical_url,
    });
    if let Some(token) = dev_token {
        body["dev_token"] = serde_json::Value::String(token);
    }
    (StatusCode::ACCEPTED, Json(body)).into_response()
}

async fn render_install_shell(headers: HeaderMap, uri: OriginalUri) -> Response {
    static_files::serve_install(uri.path(), &headers).await
}

fn token_is_valid(state: &InstallAppState, headers: &HeaderMap, query_token: Option<&str>) -> bool {
    [
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer ")),
        query_token,
        headers
            .get("x-install-token")
            .and_then(|value| value.to_str().ok()),
    ]
    .into_iter()
    .flatten()
    .any(|token| token == &*state.install_token)
}

fn require_valid_token(
    state: &InstallAppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Option<Response> {
    (!token_is_valid(state, headers, query_token))
        .then(|| ApiError::new(StatusCode::UNAUTHORIZED, "invalid install token").into_response())
}

fn observe_operator(state: &InstallAppState, headers: &HeaderMap) {
    let current = InstallOperatorFingerprint {
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string),
        remote_ip:  detect_remote_ip(headers),
    };
    if current.user_agent.is_none() && current.remote_ip.is_none() {
        return;
    }

    let mut first = lock_unpoisoned(&state.first_operator, "install operator");
    match first.as_ref() {
        None => *first = Some(current),
        Some(initial) if initial != &current => {
            warn!(
                initial_user_agent = ?initial.user_agent,
                current_user_agent = ?current.user_agent,
                initial_remote_ip = ?initial.remote_ip,
                current_remote_ip = ?current.remote_ip,
                "suspected concurrent install operators"
            );
        }
        Some(_) => {}
    }
}

fn detect_remote_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn install_github_redirect_error(state: &InstallAppState, error: &str) -> Response {
    (StatusCode::FOUND, [(
        header::LOCATION,
        format!(
            "/install/github?token={}&error={error}",
            state.install_token
        ),
    )])
        .into_response()
}

fn detect_canonical_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("http");

    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("127.0.0.1:32276");

    format!("{scheme}://{host}")
}

fn completed_steps(pending_install: &PendingInstall) -> Vec<&'static str> {
    let mut steps = Vec::new();
    if pending_install.server.is_some() {
        steps.push("server");
    }
    if pending_install.object_store.is_some() {
        steps.push("object_store");
    }
    if pending_install.llm.is_some() {
        steps.push("llm");
    }
    if pending_install.github.is_some() {
        steps.push("github");
    }
    steps
}

fn redacted_llm(pending_install: &PendingInstall) -> serde_json::Value {
    pending_install.llm.as_ref().map_or_else(
        || serde_json::Value::Null,
        |llm| {
            serde_json::json!({
                "providers": llm.providers.iter().map(|provider| serde_json::json!({
                    "provider": provider.provider.as_str(),
                    "configured": true,
                })).collect::<Vec<_>>()
            })
        },
    )
}

fn redacted_github(pending_install: &PendingInstall) -> serde_json::Value {
    pending_install.github.as_ref().map_or_else(
        || serde_json::Value::Null,
        |github| match github {
            GithubInstallState::Token(github) => serde_json::json!({
                "strategy": "token",
                "username": github.username,
            }),
            GithubInstallState::App(github) => serde_json::json!({
                "strategy": "app",
                "owner": github.owner.as_session_value(),
                "app_name": github.app_name,
                "slug": github.slug,
                "allowed_username": github.allowed_username,
            }),
        },
    )
}

fn redacted_object_store(pending_install: &PendingInstall) -> serde_json::Value {
    pending_install.object_store.as_ref().map_or_else(
        || serde_json::Value::Null,
        InstallObjectStoreState::as_session_value,
    )
}

fn missing_step_response(step: &str) -> Response {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        format!("install step '{step}' is incomplete"),
    )
    .into_response()
}

fn install_error_response(status: StatusCode, message: impl Into<String>) -> Response {
    ApiError::new(status, message).into_response()
}

#[expect(
    clippy::disallowed_types,
    reason = "Install canonical_url validation parses a public origin and rejects query/fragment credentials before storage."
)]
fn validate_canonical_url(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    let parsed = fabro_http::Url::parse(trimmed).map_err(|err| err.to_string())?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("canonical_url must use http or https, got {other}")),
    }
    if parsed.host_str().is_none() {
        return Err("canonical_url must include a host".to_string());
    }
    if trimmed.ends_with('/') {
        return Err("canonical_url must not end with a trailing slash".to_string());
    }
    if parsed.path() != "/" {
        return Err("canonical_url must not include a path".to_string());
    }
    if parsed.query().is_some() {
        return Err("canonical_url must not include a query string".to_string());
    }
    if parsed.fragment().is_some() {
        return Err("canonical_url must not include a fragment".to_string());
    }
    Ok(())
}

fn generate_ephemeral_secret() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

fn build_github_app_manifest(
    app_name: &str,
    redirect_url: &str,
    callback_url: &str,
    setup_url: &str,
) -> serde_json::Value {
    serde_json::json!({
        "name": app_name,
        "url": "https://fabro.sh",
        "redirect_url": redirect_url,
        "callback_urls": [callback_url],
        "setup_url": setup_url,
        "public": false,
        "default_permissions": {
            "contents": "write",
            "metadata": "read",
            "pull_requests": "write",
            "checks": "write",
            "issues": "write",
            "emails": "read"
        },
        "default_events": []
    })
}

#[expect(
    clippy::disallowed_types,
    reason = "Install HTTP client selection parses a public upstream base URL only to decide localhost proxy behavior."
)]
fn install_http_client_for_url(base_url: &str) -> Result<fabro_http::HttpClient, String> {
    let mut builder = fabro_http::HttpClientBuilder::new();
    if fabro_http::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
        .is_some_and(|host| host == "127.0.0.1" || host == "localhost")
    {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|err| err.to_string())
}

/// Parse and validate an install-time upstream URL.
///
/// In production the base URL is a hardcoded constant
/// (`DEFAULT_INSTALL_GITHUB_API_BASE_URL`, `DEFAULT_ANTHROPIC_BASE_URL`, …).
/// Only test code can override via
/// [`InstallAppState::with_github_api_base_url`]
/// or [`InstallAppState::with_provider_base_url`], but CodeQL sees those
/// `pub` setters as external entry points and traces taint into the
/// `format!` URL construction sites below. Passing every upstream URL
/// through this parser turns it into a typed `Url` with a verified scheme
/// and host before it is combined with a path segment.
#[expect(
    clippy::disallowed_types,
    reason = "Install upstream endpoints are raw HTTP request URLs; logging uses separate redacted boundaries."
)]
fn parse_install_upstream_url(raw: &str) -> Result<fabro_http::Url, String> {
    let url = fabro_http::Url::parse(raw).map_err(|err| err.to_string())?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "install upstream URL must use http or https, got {other}"
            ));
        }
    }
    if url.host_str().is_none() {
        return Err("install upstream URL must include a host".to_string());
    }
    Ok(url)
}

/// Append `segments` as new path segments to a validated base URL.
///
/// Each segment is percent-encoded by `url`, so caller-controlled values
/// (e.g. a GitHub manifest `code`) cannot insert additional path components,
/// alter the host, or redirect the request to a different URL scheme.
#[expect(
    clippy::disallowed_types,
    reason = "Install upstream endpoints are raw HTTP request URLs; logging uses separate redacted boundaries."
)]
fn install_upstream_endpoint(base_url: &str, segments: &[&str]) -> Result<fabro_http::Url, String> {
    let mut url = parse_install_upstream_url(base_url)?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|()| "install upstream URL cannot be a base".to_string())?;
        for segment in segments {
            path.push(segment);
        }
    }
    Ok(url)
}

async fn validate_llm_provider(
    state: &InstallAppState,
    input: &InstallLlmTestInput,
) -> Result<(), String> {
    let (auth_header, auth_value) = match input.provider {
        Provider::Anthropic => ("x-api-key", input.api_key.clone()),
        Provider::OpenAi => ("Authorization", format!("Bearer {}", input.api_key)),
        Provider::Gemini => ("x-goog-api-key", input.api_key.clone()),
        Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => {
            return Err(format!(
                "{} is not supported by install validation",
                input.provider.as_str()
            ));
        }
    };

    let base_url = provider_base_url(state, input.provider);
    let endpoint = install_upstream_endpoint(&base_url, &["models"])?;
    let client = install_http_client_for_url(&base_url)?;
    let mut request = client
        .get(endpoint)
        .header(auth_header, auth_value)
        .header("User-Agent", "fabro-server");
    if matches!(input.provider, Provider::Anthropic) {
        request = request.header("anthropic-version", "2023-06-01");
    }

    let response = request
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "{} model lookup failed ({})",
            input.provider.as_str(),
            response.status()
        ))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Install flow checks documented provider base-url overrides while building defaults."
)]
fn provider_base_url(state: &InstallAppState, provider: Provider) -> String {
    state
        .upstreams
        .provider_base_urls
        .get(&provider)
        .cloned()
        .or_else(|| match provider {
            Provider::Anthropic => std::env::var(EnvVars::ANTHROPIC_BASE_URL).ok(),
            Provider::OpenAi => std::env::var(EnvVars::OPENAI_BASE_URL).ok(),
            Provider::Gemini => std::env::var(EnvVars::GEMINI_BASE_URL).ok(),
            Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => None,
            Provider::OpenAiCompatible => std::env::var(EnvVars::OPENAI_COMPATIBLE_BASE_URL).ok(),
        })
        .unwrap_or_else(|| match provider {
            Provider::Anthropic => DEFAULT_ANTHROPIC_BASE_URL.to_string(),
            Provider::OpenAi => DEFAULT_OPENAI_BASE_URL.to_string(),
            Provider::Gemini => DEFAULT_GEMINI_BASE_URL.to_string(),
            Provider::Kimi
            | Provider::Zai
            | Provider::Minimax
            | Provider::Inception
            | Provider::OpenAiCompatible => String::new(),
        })
}

async fn validate_github_token(state: &InstallAppState, token: &str) -> Result<String, String> {
    let base_url = state
        .upstreams
        .github_api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_INSTALL_GITHUB_API_BASE_URL.to_string());
    let endpoint = install_upstream_endpoint(&base_url, &["user"])?;
    let client = install_http_client_for_url(&base_url)?;
    let response = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-server")
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub returned {}", response.status()));
    }
    let body: GithubUserResponse = response.json().await.map_err(|err| err.to_string())?;
    Ok(body.login)
}

async fn exchange_github_app_manifest_code(
    state: &InstallAppState,
    code: &str,
) -> Result<GitHubAppManifestConversion, String> {
    if !is_valid_github_manifest_code(code) {
        return Err("install GitHub manifest code is not in the expected format".to_string());
    }
    let base_url = state
        .upstreams
        .github_api_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_INSTALL_GITHUB_API_BASE_URL.to_string());
    let endpoint = install_upstream_endpoint(&base_url, &["app-manifests", code, "conversions"])?;
    let client = install_http_client_for_url(&base_url)?;
    let response = client
        .post(endpoint)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-server")
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let _ = response.text().await;
        return Err(format!("GitHub manifest conversion failed ({status})"));
    }
    response.json().await.map_err(|err| err.to_string())
}

/// GitHub's manifest-conversion `code` is short, unpadded-base64url by
/// construction. Reject anything outside that alphabet so a malicious
/// browser callback cannot smuggle extra path segments, host overrides, or
/// query parameters into the request.
fn is_valid_github_manifest_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 256
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn write_artifact_store_metadata(
    settings: &ServerSettings,
    storage_dir: &Path,
) -> anyhow::Result<()> {
    use fabro_types::settings::interp::InterpString;

    let mut settings = settings.clone();
    settings.server.storage.root = InterpString::parse(&storage_dir.display().to_string());
    let (object_store, prefix) = serve::build_artifact_object_store(&settings.server)?;
    let artifact_store = ArtifactStore::new(object_store, prefix);
    artifact_store.write_metadata(FABRO_VERSION).await?;
    Ok(())
}

struct InstallListener {
    listener: BoundInstallListener,
    bind:     Bind,
}

enum BoundInstallListener {
    Unix(UnixListener),
    Tcp(TcpListener),
}

async fn bind_install_listener(requested: &BindRequest) -> anyhow::Result<InstallListener> {
    match requested {
        BindRequest::Unix(path) => {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            let listener = UnixListener::bind(path)?;
            Ok(InstallListener {
                listener: BoundInstallListener::Unix(listener),
                bind:     Bind::Unix(path.clone()),
            })
        }
        BindRequest::Tcp(address) => {
            let listener = TcpListener::bind(address).await?;
            Ok(InstallListener {
                bind:     Bind::Tcp(listener.local_addr()?),
                listener: BoundInstallListener::Tcp(listener),
            })
        }
        BindRequest::TcpHost(host) => {
            let listener = TcpListener::bind((*host, DEFAULT_TCP_PORT)).await?;
            Ok(InstallListener {
                bind:     Bind::Tcp(listener.local_addr()?),
                listener: BoundInstallListener::Tcp(listener),
            })
        }
    }
}

async fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) {
    if *shutdown_rx.borrow() {
        return;
    }
    let _ = shutdown_rx.changed().await;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use axum::http::HeaderMap;
    use fabro_install::{OBJECT_STORE_ACCESS_KEY_ID_ENV, OBJECT_STORE_SECRET_ACCESS_KEY_ENV};
    use fabro_static::EnvVars;
    use object_store::Error as ObjectStoreError;
    use serde_json::json;

    use super::{
        DEFAULT_INSTALL_GITHUB_API_BASE_URL, InstallAppState, InstallAwsCredentialPair,
        InstallFinishGuard, InstallObjectStoreCredentialMode, InstallObjectStoreInput,
        InstallObjectStoreProvider, PendingInstall, ServerSecrets,
        classify_object_store_validation_error, detect_canonical_url, install_object_store_lookup,
        lock_unpoisoned, resolve_install_object_store_state, token_is_valid,
        write_artifact_store_metadata,
    };

    #[test]
    fn token_validation_accepts_any_matching_source() {
        let state = InstallAppState::for_test("expected");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer wrong".parse().unwrap());
        headers.insert("x-install-token", "also-wrong".parse().unwrap());

        assert!(token_is_valid(&state, &headers, Some("expected")));
    }

    #[test]
    fn token_validation_falls_back_to_custom_header() {
        let state = InstallAppState::for_test("expected");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer wrong".parse().unwrap());
        headers.insert("x-install-token", "expected".parse().unwrap());

        assert!(token_is_valid(&state, &headers, None));
    }

    #[test]
    fn canonical_url_prefers_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "fabro.example.com".parse().unwrap());

        assert_eq!(detect_canonical_url(&headers), "https://fabro.example.com");
    }

    #[test]
    fn token_validation_requires_exact_match() {
        let state = InstallAppState::for_test("expected");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer expected".parse().unwrap());
        assert!(token_is_valid(&state, &headers, None));

        headers.insert("authorization", "Bearer wrong".parse().unwrap());
        assert!(!token_is_valid(&state, &headers, None));
    }

    #[test]
    fn pending_install_lock_recovers_after_poison() {
        let pending = Arc::new(Mutex::new(PendingInstall::default()));
        let poisoned = Arc::clone(&pending);
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison install lock");
        });

        let _guard = lock_unpoisoned(&pending, "install session");
    }

    #[test]
    fn finish_guard_rejects_concurrent_finish_calls() {
        let flag = Arc::new(AtomicBool::new(false));
        let first = InstallFinishGuard::try_acquire(Arc::clone(&flag));
        assert!(first.is_some());
        assert!(InstallFinishGuard::try_acquire(Arc::clone(&flag)).is_none());
        drop(first);
        assert!(InstallFinishGuard::try_acquire(flag).is_some());
    }

    #[test]
    fn install_github_requests_default_to_fixed_github_api_base_url() {
        assert_eq!(
            DEFAULT_INSTALL_GITHUB_API_BASE_URL,
            "https://api.github.com"
        );
    }

    #[tokio::test]
    async fn write_artifact_store_metadata_creates_marker_in_overridden_storage_root() {
        use object_store::path::Path as ObjectPath;

        let dir = tempfile::tempdir().unwrap();
        let settings = fabro_config::ServerSettingsBuilder::from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
        )
        .unwrap();

        write_artifact_store_metadata(&settings, dir.path())
            .await
            .unwrap();

        let mut overridden = settings.clone();
        overridden.server.storage.root =
            fabro_types::settings::interp::InterpString::parse(&dir.path().display().to_string());
        let (object_store, prefix) =
            crate::serve::build_artifact_object_store(&overridden.server).unwrap();
        let marker = if prefix.is_empty() {
            "store-metadata.json".to_string()
        } else {
            format!("{prefix}/store-metadata.json")
        };
        let bytes = object_store
            .get(&ObjectPath::from(marker))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["fabro_version"], super::FABRO_VERSION);
    }

    #[test]
    fn resolve_install_object_store_state_rejects_local_with_s3_fields() {
        let err = resolve_install_object_store_state(None, InstallObjectStoreInput {
            provider:          InstallObjectStoreProvider::Local,
            bucket:            Some("fabro-data".to_string()),
            region:            None,
            credential_mode:   None,
            access_key_id:     None,
            secret_access_key: None,
        })
        .expect_err("local mode should reject S3-only fields");

        assert_eq!(
            err,
            "Local disk does not accept S3 bucket, region, or AWS credential fields."
        );
    }

    #[test]
    fn resolve_install_object_store_state_rejects_runtime_with_submitted_access_keys() {
        let err = resolve_install_object_store_state(None, InstallObjectStoreInput {
            provider:          InstallObjectStoreProvider::S3,
            bucket:            Some("fabro-data".to_string()),
            region:            Some("us-east-1".to_string()),
            credential_mode:   Some(InstallObjectStoreCredentialMode::Runtime),
            access_key_id:     Some("AKIA_FAKE_VALUE".to_string()),
            secret_access_key: Some("fake-secret-value".to_string()),
        })
        .expect_err("runtime mode should reject submitted access keys");

        assert_eq!(
            err,
            "AWS access key fields are only allowed when using manual AWS access key credentials."
        );
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "sync std::fs::write in a unit test fixture; not on a Tokio path"
    )]
    fn install_object_store_lookup_overrides_static_keys_and_suppresses_session_token() {
        let temp_dir = tempfile::tempdir().unwrap();
        let env_path = temp_dir.path().join("server.env");
        std::fs::write(
            &env_path,
            "\
AWS_ACCESS_KEY_ID=ambient-access\n\
AWS_SECRET_ACCESS_KEY=ambient-secret\n\
AWS_SESSION_TOKEN=ambient-session\n\
AWS_WEB_IDENTITY_TOKEN_FILE=/tmp/fabro-web-identity-token\n",
        )
        .unwrap();
        let server_secrets = ServerSecrets::load(env_path.clone(), HashMap::new()).unwrap();
        let manual_credentials =
            InstallAwsCredentialPair::new("submitted-access", "submitted-secret");

        let lookup = install_object_store_lookup(&server_secrets, Some(&manual_credentials));

        assert_eq!(
            lookup(OBJECT_STORE_ACCESS_KEY_ID_ENV).as_deref(),
            Some("submitted-access")
        );
        assert_eq!(
            lookup(OBJECT_STORE_SECRET_ACCESS_KEY_ENV).as_deref(),
            Some("submitted-secret")
        );
        assert_eq!(lookup(EnvVars::AWS_SESSION_TOKEN), None);
        assert_eq!(
            lookup(EnvVars::AWS_WEB_IDENTITY_TOKEN_FILE).as_deref(),
            Some("/tmp/fabro-web-identity-token")
        );
    }

    #[test]
    fn classify_object_store_validation_error_reports_region_mismatch() {
        let err = ObjectStoreError::Generic {
            store:  "AmazonS3",
            source: Box::new(io::Error::other(
                "Received redirect without LOCATION, this normally indicates an incorrectly configured region",
            )),
        };

        assert_eq!(
            classify_object_store_validation_error("fabro-data", "us-east-1", 0, &err),
            "Bucket fabro-data is not reachable in region us-east-1. Verify the AWS region and try again."
        );
    }

    #[test]
    fn install_secret_debug_display_and_json_are_redacted() {
        let manual_credentials =
            InstallAwsCredentialPair::new("AKIA_STRUCTURALLY_REALISTIC", "secret-value-123");

        let debug = format!("{manual_credentials:?}");
        let rendered = json!({
            "access_key_id": &manual_credentials.access_key_id,
            "secret_access_key": &manual_credentials.secret_access_key,
        })
        .to_string();

        assert!(!debug.contains("AKIA_STRUCTURALLY_REALISTIC"));
        assert!(!debug.contains("secret-value-123"));
        assert!(!rendered.contains("AKIA_STRUCTURALLY_REALISTIC"));
        assert!(!rendered.contains("secret-value-123"));
        assert!(rendered.contains("[REDACTED]"));
    }
}
