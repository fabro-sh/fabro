use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use fabro_auth::{ApiCredential, ApiKeyHeader, AuthCredential, AuthDetails, credential_id_for};
use fabro_config::{Storage, resolve_server_from_file};
use fabro_github::github_api_base_url;
use fabro_install::{
    PendingSettingsWrite, VaultSecretWrite, generate_jwt_keypair, merge_server_settings,
    persist_install_outputs_direct, write_github_app_settings, write_token_settings,
};
use fabro_llm::client::Client as LlmClient;
use fabro_llm::generate::{GenerateParams, generate};
use fabro_model::{Catalog, Provider};
use fabro_store::ArtifactStore;
use fabro_util::version::FABRO_VERSION;
use fabro_util::{Home, dev_token, session_secret};
use fabro_vault::SecretType as VaultSecretType;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};
use tower::service_fn;
use tracing::{error, info, warn};

use crate::bind::{Bind, BindRequest};
use crate::serve::{self, DEFAULT_TCP_PORT};
use crate::{security_headers, static_files};

#[derive(Clone)]
pub struct InstallAppState {
    install_token:   Arc<String>,
    pending_install: Arc<Mutex<PendingInstall>>,
    storage_dir:     Arc<PathBuf>,
    config_path:     Arc<PathBuf>,
    upstreams:       InstallUpstreamConfig,
    on_finish:       Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone, Debug, Default)]
struct InstallUpstreamConfig {
    provider_base_urls:  HashMap<Provider, String>,
    github_api_base_url: Option<String>,
}

impl InstallAppState {
    #[must_use]
    pub fn new(token: String, storage_dir: &Path, config_path: &Path) -> Self {
        Self {
            install_token:   Arc::new(token),
            pending_install: Arc::new(Mutex::new(PendingInstall::default())),
            storage_dir:     Arc::new(storage_dir.to_path_buf()),
            config_path:     Arc::new(config_path.to_path_buf()),
            upstreams:       InstallUpstreamConfig::default(),
            on_finish:       None,
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
            install_token:   Arc::new(token.to_string()),
            pending_install: Arc::new(Mutex::new(PendingInstall::default())),
            storage_dir:     Arc::new(storage_dir.to_path_buf()),
            config_path:     Arc::new(config_path.to_path_buf()),
            upstreams:       InstallUpstreamConfig::default(),
            on_finish:       None,
        }
    }

    fn with_finish_callback(self, on_finish: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            on_finish: Some(on_finish),
            ..self
        }
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
    provider:        Provider,
    api_key:         String,
    #[serde(default)]
    openai_base_url: Option<String>,
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
    provider:        Provider,
    api_key:         String,
    #[serde(default)]
    openai_base_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubTokenTestInput {
    token: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAppManifestInput {
    owner:            String,
    app_name:         String,
    allowed_username: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAppRedirectQuery {
    code:  String,
    state: String,
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
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let value = raw.trim();
        if value.eq_ignore_ascii_case("personal") {
            return Ok(Self::Personal);
        }
        if let Some(org) = value.strip_prefix("org:") {
            anyhow::ensure!(!org.trim().is_empty(), "organization owner cannot be empty");
            return Ok(Self::Organization(org.trim().to_string()));
        }
        anyhow::bail!("owner must be 'personal' or 'org:<slug>'");
    }

    fn manifest_form_action(&self) -> String {
        match self {
            Self::Personal => "https://github.com/settings/apps/new".to_string(),
            Self::Organization(org) => {
                format!("https://github.com/organizations/{org}/settings/apps/new")
            }
        }
    }

    fn as_session_value(&self) -> String {
        match self {
            Self::Personal => "personal".to_string(),
            Self::Organization(org) => format!("org:{org}"),
        }
    }
}

pub fn build_install_router(state: InstallAppState) -> Router {
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
        .fallback_service(service_fn(move |req: axum::extract::Request| async move {
            let path = req.uri().path().to_string();
            if path.starts_with("/api/") {
                Ok::<_, std::convert::Infallible>(StatusCode::NOT_FOUND.into_response())
            } else if matches!(req.method(), &Method::GET | &Method::HEAD) {
                let headers = req.headers().clone();
                Ok::<_, std::convert::Infallible>(static_files::serve_install(&path, &headers))
            } else {
                Ok::<_, std::convert::Infallible>(StatusCode::NOT_FOUND.into_response())
            }
        }))
        .layer(axum::middleware::from_fn(security_headers::layer))
}

pub async fn serve_install_command<F>(
    bind_request: BindRequest,
    state: InstallAppState,
    mut on_ready: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Bind) -> anyhow::Result<()>,
{
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let finish_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = shutdown_tx.send(true);
    });
    let state = state.with_finish_callback(finish_callback);
    let router = build_install_router(state);
    let bound_listener = bind_install_listener(&bind_request).await?;
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
        return (StatusCode::UNAUTHORIZED, "invalid install token").into_response();
    }

    let pending_install = state
        .pending_install
        .lock()
        .expect("install session lock poisoned")
        .clone();

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

    if input.api_key.trim().is_empty() {
        return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, "api_key is required");
    }
    if input.provider == Provider::OpenAiCompatible
        && input
            .openai_base_url
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return install_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "openai_base_url is required for openai_compatible",
        );
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

    if input.providers.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "at least one LLM provider is required" })),
        )
            .into_response();
    }

    for provider in &input.providers {
        if provider.api_key.trim().is_empty() {
            return install_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("api_key is required for {}", provider.provider.as_str()),
            );
        }
        if provider.provider == Provider::OpenAiCompatible
            && provider
                .openai_base_url
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return install_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "openai_base_url is required for openai_compatible",
            );
        }
    }

    state
        .pending_install
        .lock()
        .expect("install session lock poisoned")
        .llm = Some(input);
    info!("install step completed: llm");
    StatusCode::NO_CONTENT.into_response()
}

async fn put_install_server(
    State(state): State<InstallAppState>,
    headers: HeaderMap,
    Query(query): Query<InstallTokenQuery>,
    Json(input): Json<ServerConfigInput>,
) -> Response {
    if let Some(response) = require_valid_token(&state, &headers, query.token.as_deref()) {
        return response;
    }

    if input.canonical_url.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "canonical_url is required" })),
        )
            .into_response();
    }

    if let Err(err) = validate_canonical_url(&input.canonical_url) {
        return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err);
    }

    state
        .pending_install
        .lock()
        .expect("install session lock poisoned")
        .server = Some(input);
    info!("install step completed: server");
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

    if input.token.trim().is_empty() || input.username.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "token and username are required" })),
        )
            .into_response();
    }

    state
        .pending_install
        .lock()
        .expect("install session lock poisoned")
        .github = Some(GithubInstallState::Token(input));
    info!("install step completed: github_token");
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

    let owner = match GitHubAppOwner::parse(&input.owner) {
        Ok(owner) => owner,
        Err(err) => {
            return install_error_response(StatusCode::UNPROCESSABLE_ENTITY, err.to_string());
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

    let mut pending_install = state
        .pending_install
        .lock()
        .expect("install session lock poisoned");
    let Some(server) = pending_install.server.clone() else {
        return missing_step_response("server");
    };

    let state_token = match generate_ephemeral_secret() {
        Ok(token) => token,
        Err(err) => {
            return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };
    let manifest = build_github_app_manifest(
        input.app_name.trim(),
        &format!(
            "{}/install/github/app/redirect?state={state_token}",
            server.canonical_url
        ),
        &format!("{}/auth/callback/github", server.canonical_url),
        &format!("{}/setup", server.canonical_url),
    );

    pending_install.github = None;
    pending_install.pending_github_app = Some(PendingGithubApp {
        state:            state_token,
        owner:            owner.clone(),
        app_name:         input.app_name.trim().to_string(),
        allowed_username: input.allowed_username.trim().to_string(),
        expires_at:       Instant::now() + Duration::from_secs(600),
    });

    Json(serde_json::json!({
        "manifest": manifest,
        "github_form_action": owner.manifest_form_action(),
    }))
    .into_response()
}

async fn get_install_github_app_redirect(
    State(state): State<InstallAppState>,
    Query(query): Query<GithubAppRedirectQuery>,
) -> Response {
    let pending = {
        let mut pending_install = state
            .pending_install
            .lock()
            .expect("install session lock poisoned");
        let Some(pending) = pending_install.pending_github_app.take() else {
            return install_error_response(
                StatusCode::BAD_REQUEST,
                "install GitHub app state is missing",
            );
        };
        if pending.expires_at <= Instant::now() {
            return install_error_response(
                StatusCode::BAD_REQUEST,
                "install GitHub app state expired",
            );
        }
        if pending.state != query.state {
            pending_install.pending_github_app = Some(pending);
            return install_error_response(
                StatusCode::BAD_REQUEST,
                "invalid install GitHub app state",
            );
        }
        pending
    };

    match exchange_github_app_manifest_code(&state, &query.code).await {
        Ok(conversion) => {
            let mut pending_install = state
                .pending_install
                .lock()
                .expect("install session lock poisoned");
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
            info!("install step completed: github_app");
            (StatusCode::FOUND, [(
                axum::http::header::LOCATION,
                format!(
                    "/install/github/done?token={}",
                    state.install_token.as_str()
                ),
            )])
                .into_response()
        }
        Err(err) => {
            error!(error = %err, "install GitHub app exchange failed");
            install_error_response(StatusCode::BAD_GATEWAY, err)
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

    let pending_install = state
        .pending_install
        .lock()
        .expect("install session lock poisoned")
        .clone();

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
    if let Err(err) = merge_server_settings(&mut settings_doc, &server.canonical_url) {
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
        if let Some(base_url) = provider
            .openai_base_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            vault_secrets.push(VaultSecretWrite {
                name:        if provider.provider == Provider::OpenAiCompatible {
                    "OPENAI_COMPATIBLE_BASE_URL".to_string()
                } else {
                    "OPENAI_BASE_URL".to_string()
                },
                value:       base_url.to_string(),
                secret_type: VaultSecretType::Environment,
                description: None,
            });
        }
    }

    let mut server_env_secrets = Vec::new();
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
    let home = Home::from_env();
    let dev_token = match dev_token::load_or_create_dev_token(&home.dev_token_path()) {
        Ok(value) => value,
        Err(err) => {
            return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };
    if let Err(err) = dev_token::write_dev_token(
        &Storage::new(state.storage_dir.as_ref())
            .server_state()
            .dev_token_path(),
        &dev_token,
    ) {
        return install_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }

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
        ("FABRO_DEV_TOKEN".to_string(), dev_token.clone()),
    ]);

    let previous_settings = std::fs::read_to_string(state.config_path.as_ref()).ok();

    if let Err(err) = persist_install_outputs_direct(
        state.storage_dir.as_ref(),
        &server_env_secrets,
        &vault_secrets,
        Some(PendingSettingsWrite {
            path:              state.config_path.as_ref(),
            contents:          &settings_toml,
            previous_contents: previous_settings.as_deref(),
        }),
    ) {
        error!(error = %err, "install persistence failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": err.to_string(),
                "leftover_env_keys": server_env_secrets.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
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
        tokio::spawn(async move {
            sleep(Duration::from_millis(500)).await;
            on_finish();
        });
    }

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "completing",
            "restart_url": server.canonical_url,
            "dev_token": dev_token,
        })),
    )
        .into_response()
}

async fn render_install_shell(headers: HeaderMap, uri: OriginalUri) -> Response {
    static_files::serve_install(uri.path(), &headers)
}

fn token_is_valid(state: &InstallAppState, headers: &HeaderMap, query_token: Option<&str>) -> bool {
    install_token_from_request(headers, query_token)
        .is_some_and(|token| token == state.install_token.as_str())
}

fn require_valid_token(
    state: &InstallAppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Option<Response> {
    (!token_is_valid(state, headers, query_token))
        .then(|| (StatusCode::UNAUTHORIZED, "invalid install token").into_response())
}

fn install_token_from_request(headers: &HeaderMap, query_token: Option<&str>) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(ToString::to_string)
        .or_else(|| query_token.map(ToString::to_string))
        .or_else(|| {
            headers
                .get("x-install-token")
                .and_then(|value| value.to_str().ok())
                .map(ToString::to_string)
        })
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
                    "openai_base_url": provider.openai_base_url,
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
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({
            "error": format!("install step '{step}' is incomplete"),
        })),
    )
        .into_response()
}

fn install_error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

fn validate_canonical_url(value: &str) -> Result<(), String> {
    let parsed = fabro_http::Url::parse(value).map_err(|err| err.to_string())?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("canonical_url must use http or https, got {other}")),
    }
    if parsed.host_str().is_none() {
        return Err("canonical_url must include a host".to_string());
    }
    Ok(())
}

fn generate_ephemeral_secret() -> anyhow::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()))
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

async fn validate_llm_provider(
    state: &InstallAppState,
    input: &InstallLlmTestInput,
) -> Result<(), String> {
    let auth_header = if input.provider == Provider::Anthropic {
        ApiKeyHeader::Custom {
            name:  "x-api-key".to_string(),
            value: input.api_key.clone(),
        }
    } else {
        ApiKeyHeader::Bearer(input.api_key.clone())
    };

    let client = LlmClient::from_credentials(vec![ApiCredential {
        provider: input.provider,
        auth_header,
        extra_headers: HashMap::new(),
        base_url: provider_base_url(state, input.provider, input.openai_base_url.as_deref()),
        codex_mode: false,
        org_id: None,
        project_id: None,
    }])
    .await
    .map_err(|err| err.to_string())?;

    let probe_model = Catalog::builtin()
        .probe_for_provider(input.provider)
        .map_or_else(
            || format!("unknown-{}", input.provider.as_str()),
            |model| model.id.clone(),
        );

    let params = GenerateParams::new(probe_model)
        .provider(input.provider.as_str())
        .prompt("Say OK")
        .max_tokens(16)
        .client(Arc::new(client));

    timeout(Duration::from_secs(30), generate(params))
        .await
        .map_err(|_| "timeout (30s)".to_string())?
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn provider_base_url(
    state: &InstallAppState,
    provider: Provider,
    override_url: Option<&str>,
) -> Option<String> {
    override_url
        .map(ToString::to_string)
        .or_else(|| state.upstreams.provider_base_urls.get(&provider).cloned())
        .or_else(|| match provider {
            Provider::Anthropic => std::env::var("ANTHROPIC_BASE_URL").ok(),
            Provider::OpenAi => std::env::var("OPENAI_BASE_URL").ok(),
            Provider::Gemini => std::env::var("GEMINI_BASE_URL").ok(),
            Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => None,
            Provider::OpenAiCompatible => std::env::var("OPENAI_COMPATIBLE_BASE_URL").ok(),
        })
}

async fn validate_github_token(state: &InstallAppState, token: &str) -> Result<String, String> {
    let base_url = state
        .upstreams
        .github_api_base_url
        .clone()
        .unwrap_or_else(github_api_base_url);
    let client = install_http_client_for_url(&base_url)?;
    let response = client
        .get(format!("{base_url}/user"))
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
    let base_url = state
        .upstreams
        .github_api_base_url
        .clone()
        .unwrap_or_else(github_api_base_url);
    let client = install_http_client_for_url(&base_url)?;
    let response = client
        .post(format!("{base_url}/app-manifests/{code}/conversions"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-server")
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "GitHub manifest conversion failed ({status}): {body}"
        ));
    }
    response.json().await.map_err(|err| err.to_string())
}

async fn write_artifact_store_metadata(
    settings: &fabro_types::settings::SettingsLayer,
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
    use axum::http::HeaderMap;

    use super::{
        InstallAppState, detect_canonical_url, install_token_from_request, token_is_valid,
    };

    #[test]
    fn install_token_resolution_prefers_authorization_header() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer header-token".parse().unwrap());
        headers.insert("x-install-token", "header-fallback".parse().unwrap());

        assert_eq!(
            install_token_from_request(&headers, Some("query-token")).as_deref(),
            Some("header-token")
        );
    }

    #[test]
    fn install_token_resolution_falls_back_to_query_then_custom_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-install-token", "header-token".parse().unwrap());

        assert_eq!(
            install_token_from_request(&headers, Some("query-token")).as_deref(),
            Some("query-token")
        );
        assert_eq!(
            install_token_from_request(&headers, None).as_deref(),
            Some("header-token")
        );
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
}
