use std::future::{Future, IntoFuture};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Args;
use fabro_config::bind::{self, Bind, BindRequest};
use fabro_config::user::active_settings_path;
use fabro_config::{
    RunEnvironmentLayer, RunLayer, RunModelLayer, ServerLayer, ServerWebLayer, Storage,
    load_config_file, load_server_runtime_settings,
};
use fabro_install::{OBJECT_STORE_ACCESS_KEY_ID_ENV, OBJECT_STORE_SECRET_ACCESS_KEY_ENV};
use fabro_static::EnvVars;
use fabro_types::ServerSettings;
use fabro_types::settings::server::{GithubIntegrationStrategy, LogDestination, WebhookStrategy};
use fabro_types::settings::{
    GithubIntegrationSettings, InterpString, ObjectStoreSettings, ServerListenSettings,
    ServerNamespace,
};
use fabro_util::terminal::Styles;
use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey};
use object_store::client::{HttpClient, HttpConnector};
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use object_store::{ClientOptions, ObjectStore, RetryConfig};
use tokio::net::{TcpListener, UnixListener};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::canonical_origin::resolve_canonical_origin;
use crate::github_webhooks::{TailscaleFunnelManager, WEBHOOK_ROUTE, WEBHOOK_SECRET_ENV};
use crate::server::{
    AppState, AppStateConfig, ResolvedAppStateSettings, RouterOptions, build_app_state,
    build_router_with_options, reconcile_incomplete_runs_on_startup, shutdown_active_workers,
    spawn_automation_scheduler, spawn_scheduler,
};
use crate::server_secrets::{ServerSecrets, process_env_snapshot};
use crate::startup::{prepare_startup_vault, resolve_startup, validate_startup_configuration};
use crate::static_files;

pub const DEFAULT_TCP_PORT: u16 = 32276;
type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);

async fn force_exit_after_shutdown(token: CancellationToken, grace: Duration) {
    token.cancelled().await;
    sleep(grace).await;
}

async fn serve_until_shutdown<F>(
    serve_fut: F,
    shutdown: CancellationToken,
    grace: Duration,
) -> std::io::Result<()>
where
    F: IntoFuture<Output = std::io::Result<()>>,
{
    let fut = serve_fut.into_future();
    tokio::pin!(fut);
    tokio::select! {
        res = &mut fut => res,
        () = force_exit_after_shutdown(shutdown, grace) => {
            warn!(
                grace_ms = grace.as_millis(),
                "Graceful shutdown timed out; abandoning open connections"
            );
            Ok(())
        }
    }
}

fn spawn_shutdown_orchestrator_inner<S, C>(
    shutdown: CancellationToken,
    signal: S,
    cleanup: C,
) -> JoinHandle<()>
where
    S: Future<Output = ()> + Send + 'static,
    C: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        signal.await;
        shutdown.cancel();
        cleanup.await;
    })
}

fn spawn_shutdown_orchestrator(
    shutdown: CancellationToken,
    state: Arc<AppState>,
) -> JoinHandle<()> {
    let signal = async {
        shutdown_signal().await;
        set_server_title(ServerTitlePhase::Stopping, None);
    };
    let cleanup = async move {
        if let Err(err) = shutdown_active_workers(&state).await {
            error!(error = %err, "Failed to stop active workers during shutdown");
        }
    };
    spawn_shutdown_orchestrator_inner(shutdown, signal, cleanup)
}

#[derive(Debug, Clone)]
pub(crate) struct ObjectStoreBuildOptions {
    pub client_options: ClientOptions,
    pub retry_config:   RetryConfig,
}

impl Default for ObjectStoreBuildOptions {
    fn default() -> Self {
        Self {
            client_options: ClientOptions::new(),
            retry_config:   RetryConfig::default(),
        }
    }
}

/// `HttpConnector` that builds a `reqwest::Client` with `.no_proxy()`.
///
/// The object_store default `ReqwestConnector` calls `reqwest::Client::new()`,
/// which on macOS probes `SystemConfiguration` for proxies every time it runs.
/// That probe can stall long enough to blow past test timeouts. S3/MinIO
/// traffic goes directly to the configured endpoint, so skipping proxy
/// discovery is safe and keeps startup predictable.
#[derive(Debug)]
struct NoProxyReqwestConnector;

impl HttpConnector for NoProxyReqwestConnector {
    #[expect(
        clippy::disallowed_methods,
        reason = "object_store pins reqwest 0.12 and object_store::HttpClient::new requires \
                  that exact version; we can't route through fabro_http (reqwest 0.13)"
    )]
    fn connect(&self, options: &ClientOptions) -> object_store::Result<HttpClient> {
        let mut builder = object_store_reqwest::Client::builder().no_proxy();
        if let Some(raw) = options.get_config_value(&object_store::ClientConfigKey::Timeout) {
            if let Some(duration) = parse_config_duration(&raw) {
                builder = builder.timeout(duration);
            }
        }
        if let Some(raw) = options.get_config_value(&object_store::ClientConfigKey::ConnectTimeout)
        {
            if let Some(duration) = parse_config_duration(&raw) {
                builder = builder.connect_timeout(duration);
            }
        }
        let client = builder
            .build()
            .map_err(|err| object_store::Error::Generic {
                store:  "object_store",
                source: Box::new(err),
            })?;
        Ok(HttpClient::new(client))
    }
}

fn parse_config_duration(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if let Some(ms) = raw.strip_suffix("ms") {
        return ms.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(s) = raw.strip_suffix('s') {
        return s.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    None
}

#[derive(Clone, Copy)]
enum ServerTitlePhase {
    Boot,
    Listening,
    Stopping,
}

#[derive(Args, Clone)]
pub struct ServeArgs {
    /// Address to bind to (IP or IP:port for TCP, or path containing / for Unix
    /// socket)
    #[arg(long)]
    pub bind: Option<String>,

    /// Enable the embedded web UI and browser auth routes
    #[arg(long, conflicts_with = "no_web")]
    pub web: bool,

    /// Disable the embedded web UI, browser auth routes, and web-only helper
    /// endpoints
    #[arg(long, conflicts_with = "web")]
    pub no_web: bool,

    /// Override default LLM model
    #[arg(long)]
    pub model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Named environment for agent tools
    #[arg(long)]
    pub environment: Option<String>,

    /// Maximum number of concurrent run executions
    #[arg(long)]
    pub max_concurrent_runs: Option<usize>,

    /// Path to server config file (default: ~/.fabro/settings.toml)
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Run `bun run dev` in apps/fabro-web to watch/recompile web assets (debug
    /// only)
    #[cfg(debug_assertions)]
    #[arg(long)]
    pub watch_web: bool,
}

fn serve_overrides(args: &ServeArgs) -> (Option<RunLayer>, Option<ServerLayer>) {
    use fabro_types::settings::interp::InterpString;
    let mut run = RunLayer::default();
    let mut server = ServerLayer::default();
    if args.web || args.no_web {
        let web = server.web.get_or_insert_with(ServerWebLayer::default);
        web.enabled = Some(args.web);
    }
    if let Some(ref model) = args.model {
        let model_layer = run.model.get_or_insert_with(RunModelLayer::default);
        model_layer.name = Some(InterpString::parse(model));
    }
    if let Some(ref provider) = args.provider {
        let model_layer = run.model.get_or_insert_with(RunModelLayer::default);
        model_layer.provider = Some(InterpString::parse(provider));
    }
    if let Some(environment) = args.environment.as_ref() {
        let environment_layer = run
            .environment
            .get_or_insert_with(RunEnvironmentLayer::default);
        environment_layer.id = Some(environment.clone());
    }
    (
        (run != RunLayer::default()).then_some(run),
        (server != ServerLayer::default()).then_some(server),
    )
}

enum WebhookPreconditions {
    Ready {
        app_id:          String,
        private_key_pem: String,
    },
    Skip(String),
}

fn resolve_webhook_preconditions(
    github: &GithubIntegrationSettings,
    state: &Arc<AppState>,
    webhook_secret_present: bool,
) -> anyhow::Result<WebhookPreconditions> {
    if github.strategy != GithubIntegrationStrategy::App {
        return Ok(WebhookPreconditions::Skip(
            "GitHub integration auth is not set to app".to_string(),
        ));
    }
    if !webhook_secret_present {
        return Ok(WebhookPreconditions::Skip(format!(
            "{WEBHOOK_SECRET_ENV} is not set"
        )));
    }
    let Some(app_id) = github.app_id.as_ref().map(resolve_interp).transpose()? else {
        return Ok(WebhookPreconditions::Skip(
            "server.integrations.github.app_id is not set".to_string(),
        ));
    };
    let github_app = match state.github_credentials(github) {
        Ok(creds) => creds,
        Err(err) => {
            return Ok(WebhookPreconditions::Skip(format!(
                "GitHub credentials are invalid: {err}"
            )));
        }
    };
    let github_app = match github_app {
        Some(fabro_github::GitHubCredentials::App(github_app)) => github_app,
        Some(
            fabro_github::GitHubCredentials::Pat(_)
            | fabro_github::GitHubCredentials::Installation(_),
        ) => {
            return Ok(WebhookPreconditions::Skip(
                "GitHub webhooks require GitHub App credentials".to_string(),
            ));
        }
        None => {
            return Ok(WebhookPreconditions::Skip(
                "GITHUB_APP_PRIVATE_KEY is not available".to_string(),
            ));
        }
    };
    Ok(WebhookPreconditions::Ready {
        app_id,
        private_key_pem: github_app.private_key_pem,
    })
}

async fn start_webhook_strategy(
    resolved_server_settings: &ServerNamespace,
    state: &Arc<AppState>,
    bind_addr: &Bind,
    webhook_secret_present: bool,
) -> anyhow::Result<Option<TailscaleFunnelManager>> {
    let github = &resolved_server_settings.integrations.github;
    let Some(strategy) = github.webhooks.as_ref().and_then(|w| w.strategy) else {
        return Ok(None);
    };

    let (app_id, private_key_pem) =
        match resolve_webhook_preconditions(github, state, webhook_secret_present)? {
            WebhookPreconditions::Ready {
                app_id,
                private_key_pem,
            } => (app_id, private_key_pem),
            WebhookPreconditions::Skip(reason) => {
                warn!(
                    %reason,
                    "Webhook strategy is configured but skipping webhook startup"
                );
                return Ok(None);
            }
        };

    match strategy {
        WebhookStrategy::TailscaleFunnel => {
            let Some(port) = bind_addr.tcp_port() else {
                warn!(
                    "GitHub webhook strategy tailscale_funnel requires a TCP server listen address; skipping webhook startup"
                );
                return Ok(None);
            };
            match TailscaleFunnelManager::start(port, &app_id, &private_key_pem).await {
                Ok(manager) => Ok(Some(manager)),
                Err(err) => {
                    error!(
                        error = %err,
                        "Failed to start Tailscale funnel for GitHub webhooks"
                    );
                    Ok(None)
                }
            }
        }
        WebhookStrategy::ServerUrl => {
            let server_api_url = resolved_server_settings
                .api
                .url
                .as_ref()
                .map(resolve_interp)
                .transpose()?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "server.api.url must be set when webhook strategy = \"server_url\" (resolver invariant)"
                    )
                })?;
            let webhook_url = format!("{}{WEBHOOK_ROUTE}", server_api_url.trim_end_matches('/'));
            match fabro_github::update_app_webhook_config(&app_id, &private_key_pem, &webhook_url)
                .await
            {
                Ok(()) => info!(url = %webhook_url, "GitHub App webhook URL updated"),
                Err(err) => warn!(
                    error = %err,
                    url = %webhook_url,
                    "Failed to update GitHub App webhook URL"
                ),
            }
            Ok(None)
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Test-only server object-store shortcut reads a documented Fabro env var."
)]
fn use_in_memory_store() -> bool {
    !matches!(
        std::env::var(EnvVars::FABRO_TEST_IN_MEMORY_STORE)
            .ok()
            .as_deref(),
        None | Some("" | "0" | "false" | "no")
    )
}

fn build_local_object_store_with_preference(
    store_path: &Path,
    use_in_memory: bool,
) -> anyhow::Result<Arc<dyn ObjectStore>> {
    if use_in_memory {
        return Ok(Arc::new(InMemory::new()));
    }

    std::fs::create_dir_all(store_path)
        .with_context(|| format!("creating object store directory {}", store_path.display()))?;
    Ok(Arc::new(LocalFileSystem::new_with_prefix(store_path)?))
}

fn configure_s3_builder_from_env_lookup<F>(
    mut builder: AmazonS3Builder,
    env_lookup: &F,
    build_options: &ObjectStoreBuildOptions,
) -> anyhow::Result<AmazonS3Builder>
where
    F: Fn(&str) -> Option<String>,
{
    builder = builder
        .with_client_options(build_options.client_options.clone())
        .with_retry(build_options.retry_config.clone());

    let access_key_id = env_lookup(OBJECT_STORE_ACCESS_KEY_ID_ENV);
    let secret_access_key = env_lookup(OBJECT_STORE_SECRET_ACCESS_KEY_ENV);
    let session_token = env_lookup(EnvVars::AWS_SESSION_TOKEN);
    match (access_key_id, secret_access_key) {
        (Some(access_key_id), Some(secret_access_key)) => {
            builder = builder
                .with_access_key_id(access_key_id)
                .with_secret_access_key(secret_access_key);
            if let Some(session_token) = session_token {
                builder = builder.with_token(session_token);
            }
        }
        (Some(_), None) | (None, Some(_)) => {
            anyhow::bail!(
                "AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY must both be set when using static AWS credentials"
            );
        }
        (None, None) => {}
    }

    for (name, key) in [
        (
            EnvVars::AWS_WEB_IDENTITY_TOKEN_FILE,
            AmazonS3ConfigKey::WebIdentityTokenFile,
        ),
        (EnvVars::AWS_ROLE_ARN, AmazonS3ConfigKey::RoleArn),
        (
            EnvVars::AWS_ROLE_SESSION_NAME,
            AmazonS3ConfigKey::RoleSessionName,
        ),
        (
            EnvVars::AWS_ENDPOINT_URL_STS,
            AmazonS3ConfigKey::StsEndpoint,
        ),
        (
            EnvVars::AWS_CONTAINER_CREDENTIALS_RELATIVE_URI,
            AmazonS3ConfigKey::ContainerCredentialsRelativeUri,
        ),
        (
            EnvVars::AWS_CONTAINER_CREDENTIALS_FULL_URI,
            AmazonS3ConfigKey::ContainerCredentialsFullUri,
        ),
        (
            EnvVars::AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE,
            AmazonS3ConfigKey::ContainerAuthorizationTokenFile,
        ),
        (
            EnvVars::AWS_METADATA_ENDPOINT,
            AmazonS3ConfigKey::MetadataEndpoint,
        ),
        (
            EnvVars::AWS_IMDSV1_FALLBACK,
            AmazonS3ConfigKey::ImdsV1Fallback,
        ),
    ] {
        if let Some(value) = env_lookup(name) {
            builder = builder.with_config(key, value);
        }
    }

    Ok(builder)
}

pub(crate) fn build_object_store_from_settings_with_lookup<F>(
    settings: &ObjectStoreSettings,
    env_lookup: &F,
    build_options: Option<&ObjectStoreBuildOptions>,
) -> anyhow::Result<Arc<dyn ObjectStore>>
where
    F: Fn(&str) -> Option<String>,
{
    if use_in_memory_store() {
        return Ok(Arc::new(InMemory::new()));
    }

    let build_options = build_options.cloned().unwrap_or_default();
    match settings {
        ObjectStoreSettings::Local { root } => {
            build_local_object_store_with_preference(&resolve_interp_path(root)?, false)
        }
        ObjectStoreSettings::S3 {
            bucket,
            region,
            endpoint,
            path_style,
        } => {
            let mut builder = AmazonS3Builder::new()
                .with_http_connector(NoProxyReqwestConnector)
                .with_bucket_name(resolve_interp(bucket)?)
                .with_region(resolve_interp(region)?)
                .with_virtual_hosted_style_request(!*path_style);
            if let Some(endpoint) = endpoint.as_ref() {
                builder = builder.with_endpoint(resolve_interp(endpoint)?);
            }
            builder = configure_s3_builder_from_env_lookup(builder, env_lookup, &build_options)?;
            Ok(Arc::new(builder.build()?))
        }
    }
}

pub fn resolve_runtime_server_settings_for_start(
    args: &ServeArgs,
    data_dir: &Path,
) -> anyhow::Result<ServerNamespace> {
    let (run_overrides, server_overrides) = serve_overrides(args);
    let mut resolved =
        load_server_runtime_settings(args.config.as_deref(), run_overrides, server_overrides)?;
    resolved.server_settings = resolved.server_settings.with_storage_override(data_dir);
    Ok(resolved.server_settings.server)
}

fn apply_effective_log_destination(
    settings: &mut ServerSettings,
    destination: Option<LogDestination>,
) {
    if let Some(destination) = destination {
        settings.server.logging.destination = destination;
    }
}

pub fn resolve_bind_request_from_server_settings(
    settings: &ServerSettings,
    explicit_bind: Option<&str>,
) -> anyhow::Result<BindRequest> {
    match explicit_bind.map(bind::parse_bind).transpose()? {
        Some(bind) => Ok(bind),
        None => resolved_bind_request(&settings.server),
    }
}

fn resolved_bind_request(
    resolved_server_settings: &ServerNamespace,
) -> anyhow::Result<BindRequest> {
    match &resolved_server_settings.listen {
        ServerListenSettings::Unix { path } => Ok(BindRequest::Unix(resolve_interp_path(path)?)),
        ServerListenSettings::Tcp { address, .. } => Ok(BindRequest::Tcp(*address)),
    }
}

fn resolve_interp(value: &InterpString) -> anyhow::Result<String> {
    value
        .resolve(process_env_var)
        .map(|resolved| resolved.value)
        .with_context(|| format!("failed to resolve {}", value.as_source()))
}

#[expect(
    clippy::disallowed_methods,
    reason = "Server settings interpolation owns a process-env lookup facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn resolve_interp_path(value: &InterpString) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(resolve_interp(value)?))
}

fn absolute_path(path: PathBuf) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("resolving current directory for config path")?
            .join(path))
    }
}

fn load_server_secrets_for_settings(settings: &ServerNamespace) -> anyhow::Result<ServerSecrets> {
    let storage_root = resolve_interp_path(&settings.storage.root)?;
    let server_env_path = Storage::new(&storage_root).runtime_directory().env_path();
    ServerSecrets::load(server_env_path, process_env_snapshot()).map_err(anyhow::Error::from)
}

pub(crate) fn build_artifact_object_store_with_server_secrets(
    settings: &ServerNamespace,
    server_secrets: &ServerSecrets,
) -> anyhow::Result<(Arc<dyn ObjectStore>, String)> {
    let prefix = resolve_interp(&settings.artifacts.prefix)?;
    let object_store = build_object_store_from_settings_with_lookup(
        &settings.artifacts.store,
        &|name| server_secrets.get(name),
        None,
    )?;
    Ok((object_store, prefix))
}

pub fn build_artifact_object_store(
    settings: &ServerNamespace,
) -> anyhow::Result<(Arc<dyn ObjectStore>, String)> {
    let server_secrets = load_server_secrets_for_settings(settings)?;
    build_artifact_object_store_with_server_secrets(settings, &server_secrets)
}

fn build_slatedb_store_with_server_secrets(
    settings: &ServerNamespace,
    server_secrets: &ServerSecrets,
) -> anyhow::Result<(Arc<dyn ObjectStore>, String, Duration, bool)> {
    let prefix = resolve_interp(&settings.slatedb.prefix)?;
    let object_store = build_object_store_from_settings_with_lookup(
        &settings.slatedb.store,
        &|name| server_secrets.get(name),
        None,
    )?;
    Ok((
        object_store,
        prefix,
        settings.slatedb.flush_interval,
        settings.slatedb.disk_cache,
    ))
}

#[cfg(test)]
fn build_slatedb_store(
    settings: &ServerNamespace,
) -> anyhow::Result<(Arc<dyn ObjectStore>, String, Duration, bool)> {
    let server_secrets = load_server_secrets_for_settings(settings)?;
    build_slatedb_store_with_server_secrets(settings, &server_secrets)
}

/// Start the HTTP API server.
///
/// # Errors
///
/// Returns an error if the server fails to bind or encounters a fatal error.
#[allow(
    clippy::print_stderr,
    reason = "Startup warnings are operator-facing and should stay off stdout."
)]
pub async fn serve_command<F>(
    args: ServeArgs,
    styles: &'static Styles,
    storage_dir_override: Option<PathBuf>,
    effective_log_destination: Option<LogDestination>,
    mut on_ready: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Bind) -> anyhow::Result<()>,
{
    let _ = fabro_proc::title_init();
    set_server_title(ServerTitlePhase::Boot, None);

    #[cfg(debug_assertions)]
    let watch_web = args.watch_web;
    let config_path = args.config.clone();
    let active_config_path = absolute_path(active_settings_path(config_path.as_deref()))?;
    let disk_document: toml::Table = load_config_file(config_path.as_deref(), "settings.toml")?;
    let (run_overrides, server_overrides) = serve_overrides(&args);
    let mut runtime_settings = load_server_runtime_settings(
        config_path.as_deref(),
        run_overrides.clone(),
        server_overrides.clone(),
    )?;
    let disk_server_settings = runtime_settings.server_settings.server.clone();
    let data_dir = match storage_dir_override {
        Some(path) => path,
        None => resolve_interp_path(&disk_server_settings.storage.root)?,
    };
    let storage = Storage::new(&data_dir);
    let vault_path = storage.secrets_path();
    let variables_path = storage.variables_path();
    let server_env_path = storage.runtime_directory().env_path();
    runtime_settings.server_settings = runtime_settings
        .server_settings
        .with_storage_override(&data_dir);
    apply_effective_log_destination(
        &mut runtime_settings.server_settings,
        effective_log_destination,
    );
    let resolved_app_settings = ResolvedAppStateSettings {
        server_settings:       runtime_settings.server_settings,
        manifest_run_defaults: runtime_settings.manifest_run_defaults,
        llm_catalog_settings:  runtime_settings.llm_catalog_settings,
    };
    let resolved_server_settings = resolved_app_settings.server_settings.server.clone();
    validate_startup_configuration(&resolved_server_settings)?;
    let env_entries = process_env_snapshot();
    let startup_vault = prepare_startup_vault(&vault_path, &server_env_path, &env_entries)?;
    let (auth_mode, server_secrets) = resolve_startup(
        &server_env_path,
        env_entries,
        &resolved_server_settings,
        &startup_vault,
    )?;
    let webhook_secret_present = startup_vault.get(WEBHOOK_SECRET_ENV).is_some();
    let bind_request = resolve_bind_request_from_server_settings(
        &resolved_app_settings.server_settings,
        args.bind.as_deref(),
    )?;
    let shared_settings = Arc::new(RwLock::new(disk_document));
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let max_concurrent_runs = resolved_server_settings.scheduler.max_concurrent_runs;
    // In `--watch-web` mode the build watcher will populate `dist/` shortly
    // after startup. Treat that the same as assets being present so the web
    // UI is enabled from the first request rather than getting silently
    // demoted to API-only on a cold boot.
    #[cfg(debug_assertions)]
    let assume_assets_pending = watch_web;
    #[cfg(not(debug_assertions))]
    let assume_assets_pending = false;
    let web_enabled = if resolved_server_settings.web.enabled {
        if static_files::assets_available() || assume_assets_pending {
            true
        } else if args.web {
            bail!("--web requires web UI assets, but none were found");
        } else {
            warn!("Web UI assets unavailable, serving API-only mode");
            false
        }
    } else {
        false
    };
    let (object_store, slatedb_prefix, flush_interval, disk_cache) =
        build_slatedb_store_with_server_secrets(&resolved_server_settings, &server_secrets)?;
    let cache_path = if disk_cache {
        Some(storage.slatedb_cache_dir())
    } else {
        None
    };
    let store = Arc::new(fabro_store::Database::new(
        object_store,
        slatedb_prefix,
        flush_interval,
        cache_path,
    ));
    store
        .warm_projection_cache()
        .await
        .context("warming run projection cache")?;
    let auth_code_store = store.auth_codes().await?;
    let auth_token_store = store.refresh_tokens().await?;
    let (artifact_object_store, artifact_prefix) = build_artifact_object_store_with_server_secrets(
        &resolved_server_settings,
        &server_secrets,
    )?;
    let artifact_store = fabro_store::ArtifactStore::new(artifact_object_store, artifact_prefix);
    let env_lookup: EnvLookup = Arc::new(process_env_var);
    resolve_canonical_origin(&resolved_server_settings, &env_lookup).map_err(anyhow::Error::msg)?;
    let shutdown = CancellationToken::new();
    let state = build_app_state(AppStateConfig {
        resolved_settings: resolved_app_settings,
        registry_factory_override: None,
        max_concurrent_runs,
        store,
        artifact_store,
        vault_path,
        variables_path,
        preloaded_vault: Some(startup_vault),
        server_secrets,
        env_lookup,
        github_api_base_url: None,
        active_config_path,
        http_client: None,
        sandbox_provider_registry: None,
        shutdown: shutdown.clone(),
        #[cfg(test)]
        worker_control_bus: None,
        #[cfg(test)]
        worker_runtime: None,
        #[cfg(any(test, feature = "test-support"))]
        automation_materializer_override: None,
    })?;
    let reconciled = reconcile_incomplete_runs_on_startup(&state).await?;
    if reconciled > 0 {
        info!(
            reconciled_runs = reconciled,
            "Reconciled stale in-flight runs on startup"
        );
    }
    spawn_scheduler(Arc::clone(&state));
    spawn_automation_scheduler(Arc::clone(&state));
    let router = build_router_with_options(Arc::clone(&state), &auth_mode, RouterOptions {
        web_enabled,
        #[cfg(debug_assertions)]
        watch_web,
        ..RouterOptions::default()
    });
    let bound_listener = bind_listener(&bind_request).await?;
    let bind_addr = bound_listener.bind.clone();

    let webhook_manager = start_webhook_strategy(
        &resolved_server_settings,
        &state,
        &bind_addr,
        webhook_secret_present,
    )
    .await?;

    spawn_auth_store_reapers(
        Arc::clone(&auth_code_store),
        Arc::clone(&auth_token_store),
        shutdown.clone(),
    );

    // Spawn config polling task
    let state_for_poll = Arc::clone(&state);
    let shared_settings_for_poll = Arc::clone(&shared_settings);
    let config_path_for_poll = config_path.clone();
    let run_overrides_for_poll = run_overrides.clone();
    let server_overrides_for_poll = server_overrides.clone();
    let data_dir_for_poll = data_dir.clone();
    let shutdown_for_poll = shutdown.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(5));
        interval.tick().await; // skip first immediate tick
        loop {
            tokio::select! {
                () = shutdown_for_poll.cancelled() => break,
                _ = interval.tick() => {}
            }
            match load_config_file::<toml::Table>(config_path_for_poll.as_deref(), "settings.toml")
            {
                Ok(new_disk_settings) => {
                    let changed = {
                        let cfg = shared_settings_for_poll
                            .read()
                            .expect("config lock poisoned");
                        *cfg != new_disk_settings
                    };
                    if changed {
                        let resolved = load_server_runtime_settings(
                            config_path_for_poll.as_deref(),
                            run_overrides_for_poll.clone(),
                            server_overrides_for_poll.clone(),
                        )
                        .map(|mut resolved| {
                            resolved.server_settings = resolved
                                .server_settings
                                .with_storage_override(&data_dir_for_poll);
                            ResolvedAppStateSettings {
                                server_settings:       resolved.server_settings,
                                manifest_run_defaults: resolved.manifest_run_defaults,
                                llm_catalog_settings:  resolved.llm_catalog_settings,
                            }
                        });
                        match resolved {
                            Ok(resolved) => match state_for_poll.replace_runtime_settings(resolved)
                            {
                                Ok(()) => {
                                    *shared_settings_for_poll
                                        .write()
                                        .expect("config lock poisoned") = new_disk_settings;
                                    info!("Server config reloaded");
                                }
                                Err(err) => {
                                    warn!(error = %err, "Rejected reloaded server config, keeping previous");
                                }
                            },
                            Err(err) => {
                                warn!(error = %err, "Rejected reloaded server config, keeping previous");
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to reload server config, keeping previous: {e}");
                }
            }
        }
    });

    if bound_listener.used_random_port_fallback {
        if let BindRequest::TcpHost(host) = bind_request {
            warn!(
                host = %host,
                preferred_port = DEFAULT_TCP_PORT,
                "Preferred TCP port unavailable; falling back to a random port"
            );
            eprintln!(
                "{} TCP port {} is unavailable on {}; falling back to a random port.",
                styles.yellow.apply_to("Warning:"),
                DEFAULT_TCP_PORT,
                host
            );
        }
    }

    on_ready(&bind_addr)?;

    #[cfg(debug_assertions)]
    let mut watch_web_child = if watch_web {
        let web_dir = std::env::current_dir()
            .context("reading current directory for --watch-web")?
            .join("apps/fabro-web");
        info!(dir = %web_dir.display(), "Starting bun run dev (--watch-web)");
        #[expect(
            clippy::disallowed_methods,
            reason = "Debug-only --watch-web spawns a long-lived `bun run dev` child that is kill/wait'd on shutdown; std::process::Command is sufficient and avoids pulling tokio::process into this path."
        )]
        let child = std::process::Command::new("bun")
            .args(["run", "dev"])
            .current_dir(&web_dir)
            .spawn()
            .with_context(|| format!("spawning `bun run dev` in {}", web_dir.display()))?;
        Some(child)
    } else {
        None
    };

    let cleanup_handle = spawn_shutdown_orchestrator(shutdown.clone(), Arc::clone(&state));

    let serve_result = match bound_listener.listener {
        BoundListener::Unix(listener) => {
            announce_server_ready(&bind_addr, styles);
            serve_until_shutdown(
                axum::serve(listener, router).with_graceful_shutdown({
                    let token = shutdown.clone();
                    async move { token.cancelled().await }
                }),
                shutdown.clone(),
                SHUTDOWN_GRACE_PERIOD,
            )
            .await
        }
        BoundListener::Tcp(listener) => {
            announce_server_ready(&bind_addr, styles);
            serve_until_shutdown(
                axum::serve(listener, router).with_graceful_shutdown({
                    let token = shutdown.clone();
                    async move { token.cancelled().await }
                }),
                shutdown.clone(),
                SHUTDOWN_GRACE_PERIOD,
            )
            .await
        }
    };

    #[cfg(debug_assertions)]
    if let Some(ref mut child) = watch_web_child {
        let _ = child.kill();
        let _ = child.wait();
    }

    if shutdown.is_cancelled() {
        if let Err(join_err) = cleanup_handle.await {
            warn!(error = %join_err, "Shutdown orchestrator task panicked");
        }
    } else {
        cleanup_handle.abort();
    }

    serve_result?;

    if let Some(manager) = webhook_manager {
        manager.shutdown().await;
    }

    Ok(())
}

struct BoundServerListener {
    listener: BoundListener,
    bind: Bind,
    used_random_port_fallback: bool,
}

enum BoundListener {
    Unix(UnixListener),
    Tcp(TcpListener),
}

async fn bind_listener(requested: &BindRequest) -> anyhow::Result<BoundServerListener> {
    match requested {
        BindRequest::Unix(path) => {
            if path.exists() {
                std::fs::remove_file(path)
                    .with_context(|| format!("removing stale unix socket {}", path.display()))?;
            }

            let listener = UnixListener::bind(path)
                .with_context(|| format!("binding unix socket {}", path.display()))?;
            Ok(BoundServerListener {
                listener: BoundListener::Unix(listener),
                bind: Bind::Unix(path.clone()),
                used_random_port_fallback: false,
            })
        }
        BindRequest::Tcp(addr) => {
            let listener = TcpListener::bind(addr).await?;
            let resolved = listener.local_addr()?;
            Ok(BoundServerListener {
                listener: BoundListener::Tcp(listener),
                bind: Bind::Tcp(resolved),
                used_random_port_fallback: false,
            })
        }
        BindRequest::TcpHost(host) => bind_tcp_host_with_fallback(*host, DEFAULT_TCP_PORT).await,
    }
}

async fn bind_tcp_host_with_fallback(
    host: std::net::IpAddr,
    preferred_port: u16,
) -> anyhow::Result<BoundServerListener> {
    let preferred = std::net::SocketAddr::new(host, preferred_port);
    match TcpListener::bind(preferred).await {
        Ok(listener) => {
            let resolved = listener.local_addr()?;
            Ok(BoundServerListener {
                listener: BoundListener::Tcp(listener),
                bind: Bind::Tcp(resolved),
                used_random_port_fallback: false,
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            let listener = TcpListener::bind(std::net::SocketAddr::new(host, 0)).await?;
            let resolved = listener.local_addr()?;
            Ok(BoundServerListener {
                listener: BoundListener::Tcp(listener),
                bind: Bind::Tcp(resolved),
                used_random_port_fallback: true,
            })
        }
        Err(err) => Err(err.into()),
    }
}

async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        if let Err(err) = signal::ctrl_c().await {
            warn!(%err, "failed to install Ctrl+C handler; Ctrl+C will not trigger graceful shutdown");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                warn!(%err, "failed to install SIGTERM handler; SIGTERM will not trigger graceful shutdown");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    info!("Shutdown signal received, stopping server");
}

fn spawn_auth_store_reapers(
    auth_codes: Arc<fabro_store::AuthCodeStore>,
    auth_tokens: Arc<fabro_store::RefreshTokenStore>,
    shutdown: CancellationToken,
) {
    spawn_auth_code_reaper(auth_codes, shutdown.clone());
    spawn_refresh_token_reaper(auth_tokens, shutdown);
}

fn spawn_auth_code_reaper(
    auth_codes: Arc<fabro_store::AuthCodeStore>,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(30));
        interval.tick().await;

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(err) = auth_codes.gc_expired(chrono::Utc::now()).await {
                        warn!(error = %err, "Failed to garbage collect expired auth codes");
                    }
                }
            }
        }
    });
}

fn spawn_refresh_token_reaper(
    auth_tokens: Arc<fabro_store::RefreshTokenStore>,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_hours(6));
        interval.tick().await;

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    let cutoff = chrono::Utc::now() - chrono::Duration::days(7);
                    if let Err(err) = auth_tokens.gc_expired(cutoff).await {
                        warn!(error = %err, "Failed to garbage collect expired refresh tokens");
                    }
                }
            }
        }
    });
}

#[allow(
    clippy::print_stderr,
    reason = "Readiness is operator-facing startup output."
)] // Startup status belongs on stderr for operator-facing CLI output.
fn announce_server_ready(bind_addr: &Bind, styles: &'static Styles) {
    set_server_title(ServerTitlePhase::Listening, Some(bind_addr));
    info!(bind = %bind_addr, "API server started");

    eprintln!(
        "{}",
        styles.bold.apply_to(format!(
            "Fabro server listening on {}",
            styles.cyan.apply_to(bind_addr)
        )),
    );
}

fn set_server_title(phase: ServerTitlePhase, bind: Option<&Bind>) {
    fabro_proc::title_set(&server_title(phase, bind));
}

fn server_title(phase: ServerTitlePhase, bind: Option<&Bind>) -> String {
    match phase {
        ServerTitlePhase::Boot => "fabro server boot".to_string(),
        ServerTitlePhase::Listening => {
            let bind = bind.expect("listening server title requires a bind");
            format!("fabro server {}", server_bind_title(bind))
        }
        ServerTitlePhase::Stopping => "fabro server stopping".to_string(),
    }
}

fn server_bind_title(bind: &Bind) -> String {
    match bind {
        Bind::Unix(path) => format!("unix:{}", path.display()),
        Bind::Tcp(addr) => format!("tcp:{addr}"),
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_types,
    reason = "tests reserve/probe ports via sync std::net::TcpListener; the async server under \
              test uses tokio::net::TcpListener separately"
)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    use fabro_config::ServerSettingsBuilder;
    use fabro_config::bind::{Bind, BindRequest};
    use fabro_types::ServerSettings;
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::server::{LogDestination, ObjectStoreSettings};
    use fabro_util::Home;
    use tokio::time::sleep;
    use tokio_util::sync::CancellationToken;

    use super::{
        SHUTDOWN_GRACE_PERIOD, ServeArgs, ServerTitlePhase, apply_effective_log_destination,
        bind_tcp_host_with_fallback, build_local_object_store_with_preference,
        build_object_store_from_settings_with_lookup, build_slatedb_store,
        force_exit_after_shutdown, resolve_bind_request_from_server_settings, resolve_interp,
        serve_overrides, serve_until_shutdown, server_bind_title, server_title,
        spawn_shutdown_orchestrator_inner,
    };
    use crate::server::ResolvedAppStateSettings;

    fn manifest_run_defaults(source: &str) -> fabro_config::RunLayer {
        let mut document: toml::Table = source.parse().expect("v2 fixture should parse");
        document
            .remove("run")
            .map(toml::Value::try_into::<fabro_config::RunLayer>)
            .transpose()
            .expect("run settings should parse")
            .unwrap_or_default()
    }

    fn server_settings(source: &str) -> ServerSettings {
        let mut document: toml::Table = source.parse().expect("v2 fixture should parse");
        let server = document
            .entry("server")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("[server] should stay a table in test fixtures");
        let auth = server
            .entry("auth")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("[server.auth] should stay a table in test fixtures");
        auth.entry("methods").or_insert_with(|| {
            toml::Value::Array(vec![toml::Value::String("dev-token".to_string())])
        });
        ServerSettingsBuilder::from_toml(
            &toml::to_string(&document).expect("fixture should serialize"),
        )
        .expect("settings should resolve")
    }

    #[test]
    fn server_settings_interpolation_rejects_variables() {
        let err = resolve_interp(&InterpString::parse("{{ vars.STORAGE_ROOT }}")).unwrap_err();

        let rendered = format!("{err:#}");
        assert!(rendered.contains("failed to resolve {{ vars.STORAGE_ROOT }}"));
        assert!(rendered.contains("variable \"STORAGE_ROOT\""));
        assert!(rendered.contains("not supported in this interpolation context"));
    }

    fn resolved_runtime_settings(source: &str) -> ResolvedAppStateSettings {
        ResolvedAppStateSettings {
            manifest_run_defaults: manifest_run_defaults(source),
            server_settings:       server_settings(source),
            llm_catalog_settings:  fabro_model::catalog::LlmCatalogSettings::default(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn force_exit_after_shutdown_does_not_resolve_before_cancellation() {
        let token = CancellationToken::new();
        let future = force_exit_after_shutdown(token, Duration::from_secs(5));
        tokio::pin!(future);

        tokio::time::advance(Duration::from_hours(1)).await;

        assert!(matches!(futures_util::poll!(&mut future), Poll::Pending));
    }

    #[tokio::test(start_paused = true)]
    async fn force_exit_after_shutdown_resolves_grace_after_cancellation() {
        let token = CancellationToken::new();
        let grace = Duration::from_secs(5);
        let future = force_exit_after_shutdown(token.clone(), grace);
        tokio::pin!(future);

        token.cancel();
        assert!(matches!(futures_util::poll!(&mut future), Poll::Pending));

        tokio::time::advance(
            grace
                .checked_sub(Duration::from_millis(1))
                .expect("test grace should be longer than one millisecond"),
        )
        .await;
        assert!(matches!(futures_util::poll!(&mut future), Poll::Pending));

        tokio::time::advance(Duration::from_millis(1)).await;
        future.await;
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_orchestration_backstops_http_independent_of_cleanup() {
        let shutdown = CancellationToken::new();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
        let cleanup_started = Arc::new(AtomicBool::new(false));
        let cleanup_finished = Arc::new(AtomicBool::new(false));
        let cleanup_started_for_task = Arc::clone(&cleanup_started);
        let cleanup_finished_for_task = Arc::clone(&cleanup_finished);

        let cleanup_handle = spawn_shutdown_orchestrator_inner(
            shutdown.clone(),
            async move {
                signal_rx.await.expect("synthetic signal should be sent");
            },
            async move {
                cleanup_started_for_task.store(true, Ordering::SeqCst);
                sleep(Duration::from_mins(1)).await;
                cleanup_finished_for_task.store(true, Ordering::SeqCst);
            },
        );
        let serve_handle = tokio::spawn(serve_until_shutdown(
            std::future::pending::<io::Result<()>>(),
            shutdown.clone(),
            SHUTDOWN_GRACE_PERIOD,
        ));

        tokio::task::yield_now().await;
        signal_tx
            .send(())
            .expect("synthetic signal receiver should still be alive");
        tokio::task::yield_now().await;

        assert!(shutdown.is_cancelled());
        assert!(cleanup_started.load(Ordering::SeqCst));
        assert!(!serve_handle.is_finished());

        tokio::time::advance(SHUTDOWN_GRACE_PERIOD).await;
        tokio::task::yield_now().await;

        assert!(serve_handle.is_finished());
        serve_handle
            .await
            .expect("serve task should not panic")
            .expect("serve timeout should be reported as a graceful shutdown");
        assert!(!cleanup_handle.is_finished());
        assert!(!cleanup_finished.load(Ordering::SeqCst));

        tokio::time::advance(Duration::from_mins(1)).await;
        cleanup_handle
            .await
            .expect("cleanup task should finish after its own work completes");
        assert!(cleanup_finished.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn serve_call_site_aborts_cleanup_on_early_serve_error() {
        let shutdown = CancellationToken::new();
        let cleanup_handle = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        let serve_result = serve_until_shutdown(
            async { Err(io::Error::new(io::ErrorKind::AddrInUse, "listener failed")) },
            shutdown.clone(),
            SHUTDOWN_GRACE_PERIOD,
        )
        .await;

        assert!(serve_result.is_err());
        assert!(!shutdown.is_cancelled());

        if shutdown.is_cancelled() {
            panic!("early serve error should not mark shutdown as cancelled");
        } else {
            cleanup_handle.abort();
        }

        let join_err = cleanup_handle
            .await
            .expect_err("cleanup task should be aborted after early serve error");
        assert!(join_err.is_cancelled());
    }

    #[test]
    fn runtime_server_settings_preserve_storage_dir_override() {
        let mut resolved = resolved_runtime_settings("_version = 1\n");
        resolved.server_settings = resolved
            .server_settings
            .with_storage_override(&PathBuf::from("/srv/fabro-storage"));

        assert_eq!(
            resolved.server_settings.server.storage.root.as_source(),
            "/srv/fabro-storage"
        );
        let fabro_types::settings::ObjectStoreSettings::Local { root } =
            &resolved.server_settings.server.artifacts.store
        else {
            panic!("artifacts store should stay local");
        };
        assert_eq!(root.as_source(), "/srv/fabro-storage/objects/artifacts");
        let fabro_types::settings::ObjectStoreSettings::Local { root } =
            &resolved.server_settings.server.slatedb.store
        else {
            panic!("slatedb store should stay local");
        };
        assert_eq!(root.as_source(), "/srv/fabro-storage/objects/slatedb");
    }

    #[test]
    fn runtime_server_settings_keep_disk_defaults_out_of_manifest_defaults() {
        let mut resolved = resolved_runtime_settings(
            r#"
_version = 1

[server.storage]
root = "/srv/from-disk"
"#,
        );
        resolved.server_settings = resolved
            .server_settings
            .with_storage_override(&PathBuf::from("/srv/from-runtime"));

        assert_eq!(
            resolved.server_settings.server.storage.root.as_source(),
            "/srv/from-runtime"
        );
        assert_eq!(
            resolved.manifest_run_defaults,
            fabro_config::RunLayer::default(),
            "manifest defaults should stay free of server-only overrides"
        );
    }

    #[test]
    fn effective_log_destination_overrides_resolved_server_settings() {
        let mut settings = server_settings(
            r#"
_version = 1

[server.logging]
destination = "file"
"#,
        );

        apply_effective_log_destination(&mut settings, Some(LogDestination::Stdout));

        assert_eq!(settings.server.logging.destination, LogDestination::Stdout);
    }

    #[test]
    fn apply_runtime_settings_enables_web_from_cli_flag() {
        let args = ServeArgs {
            bind: None,
            model: None,
            provider: None,
            environment: None,
            web: true,
            no_web: false,
            max_concurrent_runs: None,
            config: None,
            #[cfg(debug_assertions)]
            watch_web: false,
        };

        let (_, server) = serve_overrides(&args);

        assert_eq!(
            server
                .as_ref()
                .and_then(|server| server.web.as_ref())
                .and_then(|web| web.enabled),
            Some(true)
        );
    }

    #[test]
    fn apply_runtime_settings_disables_web_from_cli_flag() {
        let args = ServeArgs {
            bind: None,
            model: None,
            provider: None,
            environment: None,
            web: false,
            no_web: true,
            max_concurrent_runs: None,
            config: None,
            #[cfg(debug_assertions)]
            watch_web: false,
        };

        let (_, server) = serve_overrides(&args);

        assert_eq!(
            server
                .as_ref()
                .and_then(|server| server.web.as_ref())
                .and_then(|web| web.enabled),
            Some(false)
        );
    }

    #[test]
    fn resolve_bind_request_from_server_settings_defaults_to_socket_when_listen_is_absent() {
        let bind =
            resolve_bind_request_from_server_settings(&server_settings("_version = 1\n"), None)
                .expect("bind");

        assert_eq!(bind, BindRequest::Unix(Home::from_env().socket_path()));
    }

    #[test]
    fn resolve_bind_request_from_server_settings_uses_configured_tcp_when_no_explicit_bind_is_given()
     {
        let settings = server_settings(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:0"
"#,
        );

        let bind = resolve_bind_request_from_server_settings(&settings, None).expect("bind");

        assert_eq!(bind, BindRequest::Tcp("127.0.0.1:0".parse().unwrap()));
    }

    #[test]
    fn resolve_bind_request_from_server_settings_prefers_explicit_bind_over_config() {
        let settings = server_settings(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"
"#,
        );

        let bind = resolve_bind_request_from_server_settings(&settings, Some("/tmp/fabro.sock"))
            .expect("bind");

        assert_eq!(bind, BindRequest::Unix(PathBuf::from("/tmp/fabro.sock")));
    }

    #[test]
    fn resolve_bind_request_from_server_settings_preserves_host_only_cli_bind() {
        let settings = server_settings("_version = 1\n");

        let bind =
            resolve_bind_request_from_server_settings(&settings, Some("127.0.0.1")).expect("bind");

        assert_eq!(bind, BindRequest::TcpHost("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn web_enabled_stays_enabled_without_github_app_mode() {
        let base = server_settings(
            r#"
_version = 1

[server.web]
enabled = true

[server.integrations.github]
strategy = "token"
"#,
        );

        let resolved = base.server;

        assert!(resolved.web.enabled);
    }

    #[test]
    fn server_title_formats_boot_listening_and_stopping() {
        let bind = Bind::Tcp("127.0.0.1:3000".parse().unwrap());

        assert_eq!(
            server_title(ServerTitlePhase::Boot, None),
            "fabro server boot"
        );
        assert_eq!(
            server_title(ServerTitlePhase::Listening, Some(&bind)),
            "fabro server tcp:127.0.0.1:3000"
        );
        assert_eq!(
            server_bind_title(&Bind::Unix(PathBuf::from("/tmp/fabro.sock"))),
            "unix:/tmp/fabro.sock"
        );
        assert_eq!(
            server_title(ServerTitlePhase::Stopping, None),
            "fabro server stopping"
        );
    }

    #[test]
    fn object_store_backend_switches_without_materializing_store_dir_for_memory() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("store");

        let disk_store = build_local_object_store_with_preference(&store_path, false)
            .expect("disk-backed store should build");
        assert!(
            store_path.exists(),
            "disk-backed store should create store dir"
        );
        drop(disk_store);

        let mem_path = temp.path().join("memory-store");
        let mem_store = build_local_object_store_with_preference(&mem_path, true)
            .expect("memory-backed store should build");
        assert!(
            !mem_path.exists(),
            "memory-backed store should not create on-disk store dir"
        );
        drop(mem_store);
    }

    #[test]
    fn build_slatedb_store_uses_configured_local_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("custom-slatedb");
        let resolved = server_settings(&format!(
            r#"
_version = 1

[server.slatedb.local]
root = "{}"
"#,
            root.display()
        ))
        .server;
        let (_object_store, prefix, flush_interval, disk_cache) =
            build_slatedb_store(&resolved).expect("slatedb store should build");

        assert!(root.exists(), "configured SlateDB root should be created");
        assert_eq!(prefix, "");
        assert_eq!(flush_interval, Duration::from_millis(1));
        assert!(!disk_cache);
    }

    #[test]
    fn build_slatedb_store_returns_disk_cache_when_enabled() {
        let resolved = server_settings(
            r"
_version = 1

[server.slatedb]
disk_cache = true
",
        )
        .server;
        let (_object_store, _prefix, _flush_interval, disk_cache) =
            build_slatedb_store(&resolved).expect("slatedb store should build");

        assert!(disk_cache);
    }

    #[test]
    fn build_object_store_from_settings_uses_injected_static_credentials() {
        let settings = ObjectStoreSettings::S3 {
            bucket:     InterpString::parse("fabro-data"),
            region:     InterpString::parse("us-east-1"),
            endpoint:   None,
            path_style: false,
        };

        let store = build_object_store_from_settings_with_lookup(
            &settings,
            &|name| match name {
                "AWS_ACCESS_KEY_ID" => Some("AKIA_TEST_VALUE".to_string()),
                "AWS_SECRET_ACCESS_KEY" => Some("secret-test-value".to_string()),
                _ => None,
            },
            None,
        );

        assert!(store.is_ok(), "injected static credentials should build");
    }

    #[test]
    fn build_object_store_from_settings_rejects_partial_static_credentials() {
        let settings = ObjectStoreSettings::S3 {
            bucket:     InterpString::parse("fabro-data"),
            region:     InterpString::parse("us-east-1"),
            endpoint:   None,
            path_style: false,
        };

        let err = build_object_store_from_settings_with_lookup(
            &settings,
            &|name| match name {
                "AWS_ACCESS_KEY_ID" => Some("AKIA_TEST_VALUE".to_string()),
                _ => None,
            },
            None,
        )
        .expect_err("partial static credentials must fail");

        assert!(
            err.to_string()
                .contains("AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY must both be set")
        );
    }

    #[test]
    fn build_object_store_from_settings_ignores_endpoint_override_env_vars() {
        let settings = ObjectStoreSettings::S3 {
            bucket:     InterpString::parse("fabro-data"),
            region:     InterpString::parse("us-east-1"),
            endpoint:   None,
            path_style: false,
        };

        let store = build_object_store_from_settings_with_lookup(
            &settings,
            &|name| match name {
                "AWS_ACCESS_KEY_ID" => Some("AKIA_TEST_VALUE".to_string()),
                "AWS_SECRET_ACCESS_KEY" => Some("secret-test-value".to_string()),
                "AWS_ENDPOINT" | "AWS_ENDPOINT_URL_S3" => {
                    Some("://not-a-valid-endpoint".to_string())
                }
                _ => None,
            },
            None,
        );

        assert!(
            store.is_ok(),
            "unsupported endpoint env vars should be ignored"
        );
    }

    #[tokio::test]
    async fn tcp_host_request_uses_preferred_port_when_available() {
        let preferred = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = preferred.local_addr().unwrap().port();
        drop(preferred);

        let bound = bind_tcp_host_with_fallback("127.0.0.1".parse().unwrap(), port)
            .await
            .unwrap();
        let resolved = match bound.bind {
            Bind::Tcp(addr) => addr,
            Bind::Unix(_) => panic!("expected tcp bind"),
        };
        assert_eq!(
            resolved,
            std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), port)
        );
        assert!(
            !bound.used_random_port_fallback,
            "preferred port should be used when available"
        );
    }

    #[tokio::test]
    async fn tcp_host_request_falls_back_when_preferred_port_is_occupied() {
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();
        let bound = bind_tcp_host_with_fallback("127.0.0.1".parse().unwrap(), occupied_port)
            .await
            .unwrap();

        let resolved = match bound.bind {
            Bind::Tcp(addr) => addr,
            Bind::Unix(_) => panic!("expected tcp bind"),
        };

        assert_ne!(resolved.port(), occupied_port);
        assert!(bound.used_random_port_fallback);
    }
}
