use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use fabro_config::bind::{self, Bind, BindRequest};
use fabro_config::{
    RunLayer, RunModelLayer, RunSandboxLayer, ServerLayer, ServerWebLayer, Storage,
    load_config_file, load_server_runtime_settings,
};
use fabro_install::{OBJECT_STORE_ACCESS_KEY_ID_ENV, OBJECT_STORE_SECRET_ACCESS_KEY_ENV};
use fabro_sandbox::SandboxProvider;
use fabro_types::ServerSettings;
use fabro_types::settings::server::{GithubIntegrationStrategy, WebhookStrategy};
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
use tokio::sync::watch;
use tokio::time::interval;
use tracing::{error, info, warn};

use crate::canonical_origin::resolve_canonical_origin;
use crate::github_webhooks::{TailscaleFunnelManager, WEBHOOK_ROUTE, WEBHOOK_SECRET_ENV};
use crate::ip_allowlist::{GitHubMetaResolver, IpAllowlistConfig, resolve_ip_allowlist_config};
use crate::server::{
    AppState, AppStateConfig, ResolvedAppStateSettings, RouterOptions, build_app_state,
    build_router_with_options, reconcile_incomplete_runs_on_startup, shutdown_active_workers,
    spawn_scheduler,
};
use crate::server_secrets::{ServerSecrets, process_env_snapshot};
use crate::startup::resolve_startup;

const TEST_IN_MEMORY_STORE_ENV: &str = "FABRO_TEST_IN_MEMORY_STORE";
const AWS_SESSION_TOKEN_ENV: &str = "AWS_SESSION_TOKEN";
pub const DEFAULT_TCP_PORT: u16 = 32276;
type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

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

    /// Sandbox for agent tools
    #[arg(long, value_enum)]
    pub sandbox: Option<SandboxProvider>,

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
    if let Some(sandbox) = args.sandbox {
        let sandbox_layer = run.sandbox.get_or_insert_with(RunSandboxLayer::default);
        sandbox_layer.provider = Some(sandbox.to_string());
    }
    (
        (run != RunLayer::default()).then_some(run),
        (server != ServerLayer::default()).then_some(server),
    )
}

async fn resolve_github_webhook_ip_allowlist(
    resolved_server_settings: &ServerNamespace,
    github_meta_resolver: &GitHubMetaResolver,
) -> anyhow::Result<Arc<IpAllowlistConfig>> {
    let config = resolve_ip_allowlist_config(
        &resolved_server_settings.ip_allowlist,
        resolved_server_settings
            .integrations
            .github
            .webhooks
            .as_ref()
            .and_then(|webhooks| webhooks.ip_allowlist.as_ref()),
        github_meta_resolver,
    )
    .await
    .context("resolving GitHub webhook IP allowlist")?;

    Ok(Arc::new(config))
}

async fn resolve_startup_github_webhook_ip_allowlist(
    resolved_server_settings: &ServerNamespace,
    github_meta_resolver: &GitHubMetaResolver,
    webhook_secret_present: bool,
) -> anyhow::Result<Option<Arc<IpAllowlistConfig>>> {
    if !webhook_secret_present {
        return Ok(None);
    }

    resolve_github_webhook_ip_allowlist(resolved_server_settings, github_meta_resolver)
        .await
        .map(Some)
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
    let Some(fabro_github::GitHubCredentials::App(github_app)) = github_app else {
        return Ok(WebhookPreconditions::Skip(
            "GITHUB_APP_PRIVATE_KEY is not available".to_string(),
        ));
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

fn use_in_memory_store() -> bool {
    !matches!(
        std::env::var(TEST_IN_MEMORY_STORE_ENV).ok().as_deref(),
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
    let session_token = env_lookup(AWS_SESSION_TOKEN_ENV);
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
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            AmazonS3ConfigKey::WebIdentityTokenFile,
        ),
        ("AWS_ROLE_ARN", AmazonS3ConfigKey::RoleArn),
        ("AWS_ROLE_SESSION_NAME", AmazonS3ConfigKey::RoleSessionName),
        ("AWS_ENDPOINT_URL_STS", AmazonS3ConfigKey::StsEndpoint),
        (
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            AmazonS3ConfigKey::ContainerCredentialsRelativeUri,
        ),
        (
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            AmazonS3ConfigKey::ContainerCredentialsFullUri,
        ),
        (
            "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
            AmazonS3ConfigKey::ContainerAuthorizationTokenFile,
        ),
        ("AWS_METADATA_ENDPOINT", AmazonS3ConfigKey::MetadataEndpoint),
        ("AWS_IMDSV1_FALLBACK", AmazonS3ConfigKey::ImdsV1Fallback),
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
        .resolve(|name| std::env::var(name).ok())
        .map(|resolved| resolved.value)
        .with_context(|| format!("failed to resolve {}", value.as_source()))
}

fn resolve_interp_path(value: &InterpString) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(resolve_interp(value)?))
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
    let server_env_path = storage.runtime_directory().env_path();
    runtime_settings.server_settings = runtime_settings
        .server_settings
        .with_storage_override(&data_dir);
    let resolved_app_settings = ResolvedAppStateSettings {
        server_settings:       runtime_settings.server_settings,
        manifest_run_defaults: runtime_settings.manifest_run_defaults,
        manifest_run_settings: runtime_settings.manifest_run_settings,
    };
    let resolved_server_settings = resolved_app_settings.server_settings.server.clone();
    let (auth_mode, server_secrets) = resolve_startup(
        &server_env_path,
        process_env_snapshot(),
        &resolved_server_settings,
    )?;
    let webhook_secret_present = server_secrets.get(WEBHOOK_SECRET_ENV).is_some();
    let bind_request = resolve_bind_request_from_server_settings(
        &resolved_app_settings.server_settings,
        args.bind.as_deref(),
    )?;
    let shared_settings = Arc::new(RwLock::new(disk_document));
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let max_concurrent_runs = resolved_server_settings.scheduler.max_concurrent_runs;
    let web_enabled = resolved_server_settings.web.enabled;
    let github_meta_resolver = GitHubMetaResolver::from_cache_dir(&storage.cache_dir())?;

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
    let auth_code_store = store.auth_codes().await?;
    let auth_token_store = store.refresh_tokens().await?;
    let (artifact_object_store, artifact_prefix) = build_artifact_object_store_with_server_secrets(
        &resolved_server_settings,
        &server_secrets,
    )?;
    let artifact_store = fabro_store::ArtifactStore::new(artifact_object_store, artifact_prefix);
    let env_lookup: EnvLookup = Arc::new(|name| std::env::var(name).ok());
    resolve_canonical_origin(&resolved_server_settings, &env_lookup).map_err(anyhow::Error::msg)?;
    let state = build_app_state(AppStateConfig {
        resolved_settings: resolved_app_settings,
        registry_factory_override: None,
        max_concurrent_runs,
        store,
        artifact_store,
        vault_path,
        server_secrets,
        env_lookup,
        github_api_base_url: None,
        http_client: None,
    })?;
    let reconciled = reconcile_incomplete_runs_on_startup(&state).await?;
    if reconciled > 0 {
        info!(
            reconciled_runs = reconciled,
            "Reconciled stale in-flight runs on startup"
        );
    }
    spawn_scheduler(Arc::clone(&state));
    let default_ip_allowlist = Arc::new(
        resolve_ip_allowlist_config(
            &resolved_server_settings.ip_allowlist,
            None,
            &github_meta_resolver,
        )
        .await
        .context("resolving server IP allowlist")?,
    );
    let github_webhook_ip_allowlist = resolve_startup_github_webhook_ip_allowlist(
        &resolved_server_settings,
        &github_meta_resolver,
        webhook_secret_present,
    )
    .await?;
    let router = build_router_with_options(
        Arc::clone(&state),
        &auth_mode,
        Arc::clone(&default_ip_allowlist),
        RouterOptions {
            web_enabled,
            github_webhook_ip_allowlist,
            ..RouterOptions::default()
        },
    );
    let bound_listener = bind_listener(&bind_request).await?;
    let bind_addr = bound_listener.bind.clone();

    let webhook_manager = start_webhook_strategy(
        &resolved_server_settings,
        &state,
        &bind_addr,
        webhook_secret_present,
    )
    .await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_state = Arc::clone(&state);
    tokio::spawn(async move {
        shutdown_signal().await;
        set_server_title(ServerTitlePhase::Stopping, None);
        if let Err(err) = shutdown_active_workers(&shutdown_state).await {
            error!(error = %err, "Failed to stop active workers during shutdown");
        }
        let _ = shutdown_tx.send(true);
    });

    spawn_auth_store_reapers(
        Arc::clone(&auth_code_store),
        Arc::clone(&auth_token_store),
        shutdown_rx.clone(),
    );

    // Spawn config polling task
    let state_for_poll = Arc::clone(&state);
    let shared_settings_for_poll = Arc::clone(&shared_settings);
    let config_path_for_poll = config_path.clone();
    let run_overrides_for_poll = run_overrides.clone();
    let server_overrides_for_poll = server_overrides.clone();
    let data_dir_for_poll = data_dir.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(5));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
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
                                manifest_run_settings: resolved.manifest_run_settings,
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

    match bound_listener.listener {
        BoundListener::Unix(listener) => {
            announce_server_ready(&bind_addr, styles);
            axum::serve(listener, router)
                .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()))
                .await?;
        }
        BoundListener::Tcp(listener) => {
            announce_server_ready(&bind_addr, styles);
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()))
            .await?;
        }
    }

    #[cfg(debug_assertions)]
    if let Some(ref mut child) = watch_web_child {
        let _ = child.kill();
        let _ = child.wait();
    }

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
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    info!("Shutdown signal received, stopping server");
}

async fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) {
    if *shutdown_rx.borrow() {
        return;
    }
    let _ = shutdown_rx.changed().await;
}

fn spawn_auth_store_reapers(
    auth_codes: Arc<fabro_store::AuthCodeStore>,
    auth_tokens: Arc<fabro_store::RefreshTokenStore>,
    shutdown_rx: watch::Receiver<bool>,
) {
    spawn_auth_code_reaper(auth_codes, shutdown_rx.clone());
    spawn_refresh_token_reaper(auth_tokens, shutdown_rx);
}

fn spawn_auth_code_reaper(
    auth_codes: Arc<fabro_store::AuthCodeStore>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(30));
        interval.tick().await;

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => break,
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
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_hours(6));
        interval.tick().await;

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => break,
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
    use std::path::PathBuf;
    use std::time::Duration;

    use fabro_config::bind::{Bind, BindRequest};
    use fabro_config::{RunSettingsBuilder, ServerSettingsBuilder};
    use fabro_types::ServerSettings;
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::server::ObjectStoreSettings;
    use fabro_util::Home;

    use super::{
        GitHubMetaResolver, ServeArgs, ServerTitlePhase, bind_tcp_host_with_fallback,
        build_local_object_store_with_preference, build_object_store_from_settings_with_lookup,
        build_slatedb_store, resolve_bind_request_from_server_settings,
        resolve_github_webhook_ip_allowlist, resolve_startup_github_webhook_ip_allowlist,
        serve_overrides, server_bind_title, server_title,
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

    fn resolved_runtime_settings(source: &str) -> ResolvedAppStateSettings {
        let manifest_run_defaults = manifest_run_defaults(source);
        ResolvedAppStateSettings {
            manifest_run_settings: RunSettingsBuilder::from_run_layer(&manifest_run_defaults)
                .map_err(|err| err.to_string()),
            manifest_run_defaults,
            server_settings: server_settings(source),
        }
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
    fn apply_runtime_settings_enables_web_from_cli_flag() {
        let args = ServeArgs {
            bind: None,
            model: None,
            provider: None,
            sandbox: None,
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
            sandbox: None,
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

    #[tokio::test]
    async fn resolve_github_webhook_ip_allowlist_propagates_resolution_errors() {
        let settings = server_settings(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:0"

[server.integrations.github]
strategy = "app"
app_id = "123"

[server.integrations.github.webhooks.ip_allowlist]
entries = ["github_meta_hooks"]
"#,
        )
        .server;

        let cache_dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let resolver = GitHubMetaResolver::new(
            fabro_http::test_http_client().unwrap(),
            format!("http://127.0.0.1:{port}/meta"),
            cache_dir.path().join("github-meta.json"),
        );

        let error = resolve_github_webhook_ip_allowlist(&settings, &resolver)
            .await
            .expect_err("github webhook allowlist resolution should fail closed");

        assert!(error.to_string().contains("GitHub webhook IP allowlist"));
    }

    #[tokio::test]
    async fn resolve_startup_github_webhook_ip_allowlist_skips_resolution_without_webhook_secret() {
        let settings = server_settings(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:0"

[server.integrations.github]
strategy = "app"
app_id = "123"

[server.integrations.github.webhooks.ip_allowlist]
entries = ["github_meta_hooks"]
"#,
        )
        .server;

        let cache_dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let resolver = GitHubMetaResolver::new(
            fabro_http::test_http_client().unwrap(),
            format!("http://127.0.0.1:{port}/meta"),
            cache_dir.path().join("github-meta.json"),
        );

        let allowlist = resolve_startup_github_webhook_ip_allowlist(&settings, &resolver, false)
            .await
            .expect("inactive webhook route should skip GitHub meta resolution");

        assert!(allowlist.is_none());
    }
}
