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
use fabro_config::{Storage, resolve_server_from_file};
use fabro_install::{
    InstallListenConfig, PendingSettingsWrite, VaultSecretWrite, generate_jwt_keypair,
    merge_server_settings, persist_install_outputs_direct, write_github_app_settings,
    write_token_settings,
};
use fabro_model::Provider;
use fabro_store::ArtifactStore;
use fabro_types::settings::SettingsLayer;
use fabro_util::version::FABRO_VERSION;
use fabro_util::{Home, dev_token, session_secret};
use fabro_vault::SecretType as VaultSecretType;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch;
use tokio::time::sleep;
use tower::service_fn;
use tracing::{error, info, warn};

use crate::bind::{Bind, BindRequest};
use crate::error::ApiError;
use crate::serve::{self, DEFAULT_TCP_PORT};
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
    pub fn for_test_with_paths(token: &str, storage_dir: &Path, config_path: &Path) -> Self {
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

    let Some(llm) = pending_install.llm else {
        return missing_step_response("llm");
    };
    let Some(server) = pending_install.server else {
        return missing_step_response("server");
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

    let mut server_env_secrets = Vec::new();
    let mut dev_token: Option<String> = None;
    match github {
        GithubInstallState::Token(github) => {
            if let Err(err) = write_token_settings(&mut settings_doc) {
                return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
            }
            vault_secrets.push(VaultSecretWrite {
                name:        "GITHUB_TOKEN".to_string(),
                value:       github.token,
                secret_type: VaultSecretType::Environment,
                description: None,
            });
            let home = state.home.clone().unwrap_or_else(Home::from_env);
            let token = match dev_token::load_or_create_dev_token(&home.dev_token_path()) {
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
                    .runtime_state()
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
            server_env_secrets.push((
                "GITHUB_APP_PRIVATE_KEY".to_string(),
                BASE64_STANDARD.encode(github.pem.as_bytes()),
            ));
            server_env_secrets.push(("GITHUB_APP_CLIENT_SECRET".to_string(), github.client_secret));
            if let Some(secret) = github.webhook_secret {
                server_env_secrets.push(("GITHUB_APP_WEBHOOK_SECRET".to_string(), secret));
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
    let (jwt_private_pem, jwt_public_pem) = match generate_jwt_keypair() {
        Ok(value) => value,
        Err(err) => {
            return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };
    server_env_secrets.extend([
        (
            "FABRO_JWT_PRIVATE_KEY".to_string(),
            BASE64_STANDARD.encode(jwt_private_pem.as_bytes()),
        ),
        (
            "FABRO_JWT_PUBLIC_KEY".to_string(),
            BASE64_STANDARD.encode(jwt_public_pem.as_bytes()),
        ),
        ("SESSION_SECRET".to_string(), session_secret),
    ]);
    if let Some(token) = dev_token.as_ref() {
        server_env_secrets.push(("FABRO_DEV_TOKEN".to_string(), token.clone()));
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "install-finish handler: reads current settings file once to produce a rollback \
                  snapshot before writing the new settings; one-shot per install-finish request"
    )]
    let previous_settings = std::fs::read_to_string(state.config_path.as_ref()).ok();

    if let Err(err) = persist_install_outputs_direct(
        state.storage_dir.as_ref(),
        &server_env_secrets,
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
        let leftover_env_keys: Vec<String> = server_env_secrets
            .iter()
            .map(|(key, _)| key.clone())
            .collect();
        return (
            status,
            Json(serde_json::json!({
                "errors": [{
                    "status": status.as_u16().to_string(),
                    "title": title,
                    "detail": detail,
                }],
                "leftover_env_keys": leftover_env_keys,
            })),
        )
            .into_response();
    }

    if let Ok(settings) = fabro_config::parse_settings_layer(&settings_toml) {
        if let Err(err) = write_artifact_store_metadata(&settings, state.storage_dir.as_ref()).await
        {
            warn!(error = %err, "failed to write artifact store metadata after install");
        }
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
    if pending_install.llm.is_some() {
        steps.push("llm");
    }
    if pending_install.server.is_some() {
        steps.push("server");
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

fn provider_base_url(state: &InstallAppState, provider: Provider) -> String {
    state
        .upstreams
        .provider_base_urls
        .get(&provider)
        .cloned()
        .or_else(|| match provider {
            Provider::Anthropic => std::env::var("ANTHROPIC_BASE_URL").ok(),
            Provider::OpenAi => std::env::var("OPENAI_BASE_URL").ok(),
            Provider::Gemini => std::env::var("GEMINI_BASE_URL").ok(),
            Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => None,
            Provider::OpenAiCompatible => std::env::var("OPENAI_COMPATIBLE_BASE_URL").ok(),
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
    settings: &SettingsLayer,
    storage_dir: &Path,
) -> anyhow::Result<()> {
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::server::{ServerLayer, ServerStorageLayer};

    let mut settings = settings.clone();
    let server = settings.server.get_or_insert_with(ServerLayer::default);
    let storage = server
        .storage
        .get_or_insert_with(ServerStorageLayer::default);
    storage.root = Some(InterpString::parse(&storage_dir.display().to_string()));

    let resolved = resolve_server_from_file(&settings).map_err(|errors| {
        anyhow::anyhow!(
            "failed to resolve server settings:\n{}",
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;
    let (object_store, prefix) = serve::build_artifact_object_store(&resolved)?;
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
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use axum::http::HeaderMap;

    use super::{
        DEFAULT_INSTALL_GITHUB_API_BASE_URL, InstallAppState, InstallFinishGuard, PendingInstall,
        detect_canonical_url, lock_unpoisoned, token_is_valid,
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
}
