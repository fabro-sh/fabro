use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use fabro_config::merge::combine_files;
use fabro_config::user::load_settings_config;
use fabro_config::{Storage, resolve_server_from_file};
use fabro_sandbox::SandboxProvider;
use fabro_types::settings::server::{
    GithubIntegrationStrategy, ServerLayer, ServerListenLayer, WebhookStrategy,
};
use fabro_types::settings::{
    GithubIntegrationSettings, InterpString, ObjectStoreSettings, ServerListenSettings,
    ServerSettings as ResolvedServerSettings, SettingsLayer,
};
use fabro_util::terminal::Styles;
use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch;
use tokio::time::interval;
use tracing::{error, info, warn};

use crate::bind::{self, Bind, BindRequest};
use crate::github_webhooks::{TailscaleFunnelManager, WEBHOOK_ROUTE, WEBHOOK_SECRET_ENV};
use crate::ip_allowlist::{GitHubMetaResolver, IpAllowlistConfig, resolve_ip_allowlist_config};
use crate::jwt_auth::resolve_auth_mode_with_lookup;
use crate::server::{
    AppState, AppStateConfig, RouterOptions, build_app_state, build_router_with_options,
    reconcile_incomplete_runs_on_startup, shutdown_active_workers, spawn_scheduler,
};
use crate::server_secrets::ServerSecrets;

const TEST_IN_MEMORY_STORE_ENV: &str = "FABRO_TEST_IN_MEMORY_STORE";
pub const DEFAULT_TCP_PORT: u16 = 32276;
type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

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

fn load_settings(path: Option<&Path>) -> anyhow::Result<SettingsLayer> {
    Ok(load_settings_config(path)?)
}

fn apply_serve_overrides(base: &SettingsLayer, args: &ServeArgs) -> SettingsLayer {
    use fabro_types::settings::cli::CliLayer;
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::run::{RunLayer, RunModelLayer, RunSandboxLayer};
    use fabro_types::settings::server::{ServerLayer, ServerWebLayer};
    let mut settings = base.clone();
    if args.web || args.no_web {
        let server = settings.server.get_or_insert_with(ServerLayer::default);
        let web = server.web.get_or_insert_with(ServerWebLayer::default);
        web.enabled = Some(args.web);
    }
    if let Some(ref model) = args.model {
        let run = settings.run.get_or_insert_with(RunLayer::default);
        let model_layer = run.model.get_or_insert_with(RunModelLayer::default);
        model_layer.name = Some(InterpString::parse(model));
    }
    if let Some(ref provider) = args.provider {
        let run = settings.run.get_or_insert_with(RunLayer::default);
        let model_layer = run.model.get_or_insert_with(RunModelLayer::default);
        model_layer.provider = Some(InterpString::parse(provider));
    }
    if let Some(sandbox) = args.sandbox {
        let run = settings.run.get_or_insert_with(RunLayer::default);
        let sandbox_layer = run.sandbox.get_or_insert_with(RunSandboxLayer::default);
        sandbox_layer.provider = Some(sandbox.to_string());
    }
    // CliLayer is namespaced; nothing to populate from flag overrides today.
    let _ = CliLayer::default();
    settings
}

fn apply_runtime_settings(
    base: &SettingsLayer,
    args: &ServeArgs,
    data_dir: &Path,
) -> SettingsLayer {
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::server::{ServerLayer, ServerStorageLayer};
    let mut settings = apply_serve_overrides(base, args);
    let server = settings.server.get_or_insert_with(ServerLayer::default);
    let storage = server
        .storage
        .get_or_insert_with(ServerStorageLayer::default);
    storage.root = Some(InterpString::parse(&data_dir.to_string_lossy()));
    settings
}

fn router_web_enabled(settings: &ResolvedServerSettings) -> bool {
    settings.web.enabled
}

async fn resolve_github_webhook_ip_allowlist(
    resolved_server_settings: &ResolvedServerSettings,
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
    resolved_server_settings: &ResolvedServerSettings,
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
    resolved_server_settings: &ResolvedServerSettings,
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

fn build_object_store_from_settings(
    settings: &ObjectStoreSettings,
) -> anyhow::Result<Arc<dyn ObjectStore>> {
    if use_in_memory_store() {
        return Ok(Arc::new(InMemory::new()));
    }

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
            let mut builder = AmazonS3Builder::from_env()
                .with_bucket_name(resolve_interp(bucket)?)
                .with_region(resolve_interp(region)?)
                .with_virtual_hosted_style_request(!*path_style);
            if let Some(endpoint) = endpoint.as_ref() {
                builder = builder.with_endpoint(resolve_interp(endpoint)?);
            }
            Ok(Arc::new(builder.build()?))
        }
    }
}

fn resolve_server_settings(file: &SettingsLayer) -> anyhow::Result<ResolvedServerSettings> {
    resolve_server_from_file(file).map_err(|errors| {
        anyhow::anyhow!(
            "failed to resolve server settings:\n{}",
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    })
}

pub fn resolve_bind_request_from_settings(
    settings: &SettingsLayer,
    explicit_bind: Option<&str>,
) -> anyhow::Result<BindRequest> {
    let effective_settings = match explicit_bind.map(bind::parse_bind).transpose()? {
        Some(BindRequest::TcpHost(host)) => return Ok(BindRequest::TcpHost(host)),
        Some(bind) => combine_files(settings.clone(), bind_override_layer(bind)),
        None => settings.clone(),
    };
    let resolved = resolve_server_settings(&effective_settings)?;
    resolved_bind_request(&resolved)
}

fn bind_override_layer(bind: BindRequest) -> SettingsLayer {
    let listen = match bind {
        BindRequest::Unix(path) => ServerListenLayer::Unix {
            path: Some(InterpString::parse(&path.display().to_string())),
        },
        BindRequest::Tcp(address) => ServerListenLayer::Tcp {
            address: Some(InterpString::parse(&address.to_string())),
        },
        BindRequest::TcpHost(_) => {
            unreachable!("host-only bind requests are handled before building a settings override")
        }
    };

    SettingsLayer {
        server: Some(ServerLayer {
            listen: Some(listen),
            ..ServerLayer::default()
        }),
        ..SettingsLayer::default()
    }
}

fn resolved_bind_request(
    resolved_server_settings: &ResolvedServerSettings,
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

pub fn build_artifact_object_store(
    settings: &ResolvedServerSettings,
) -> anyhow::Result<(Arc<dyn ObjectStore>, String)> {
    let prefix = resolve_interp(&settings.artifacts.prefix)?;
    let object_store = build_object_store_from_settings(&settings.artifacts.store)?;
    Ok((object_store, prefix))
}

fn build_slatedb_store(
    settings: &ResolvedServerSettings,
) -> anyhow::Result<(Arc<dyn ObjectStore>, String, Duration, bool)> {
    let prefix = resolve_interp(&settings.slatedb.prefix)?;
    let object_store = build_object_store_from_settings(&settings.slatedb.store)?;
    Ok((
        object_store,
        prefix,
        settings.slatedb.flush_interval,
        settings.slatedb.disk_cache,
    ))
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
    let disk_settings = load_settings(config_path.as_deref())?;
    let disk_server_settings = resolve_server_settings(&disk_settings)?;
    let data_dir = match storage_dir_override {
        Some(path) => path,
        None => resolve_interp_path(&disk_server_settings.storage.root)?,
    };
    let storage = Storage::new(&data_dir);
    let vault_path = storage.secrets_path();
    let server_env_path = storage.server_state().env_path();
    let server_secrets = ServerSecrets::load(server_env_path.clone())?;
    let webhook_secret_present = server_secrets.get(WEBHOOK_SECRET_ENV).is_some();

    // Shared config for live reloading
    let effective_settings = apply_runtime_settings(&disk_settings, &args, &data_dir);
    let resolved_server_settings = resolve_server_settings(&effective_settings)?;
    let bind_request =
        resolve_bind_request_from_settings(&effective_settings, args.bind.as_deref())?;
    let shared_settings = Arc::new(RwLock::new(effective_settings));
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let (auth_mode, max_concurrent_runs) = {
        let auth_mode = resolve_auth_mode_with_lookup(&resolved_server_settings, |name| {
            server_secrets.get(name)
        })?;
        let max_concurrent_runs = resolved_server_settings.scheduler.max_concurrent_runs;
        (auth_mode, max_concurrent_runs)
    };
    let web_enabled = router_web_enabled(&resolved_server_settings);
    let github_meta_resolver = GitHubMetaResolver::from_cache_dir(&storage.cache_dir())?;

    let (object_store, slatedb_prefix, flush_interval, disk_cache) =
        build_slatedb_store(&resolved_server_settings)?;
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
    let auth_token_store = store.auth_tokens().await?;
    let (artifact_object_store, artifact_prefix) =
        build_artifact_object_store(&resolved_server_settings)?;
    let artifact_store = fabro_store::ArtifactStore::new(artifact_object_store, artifact_prefix);
    let env_lookup: EnvLookup = Arc::new(|name| std::env::var(name).ok());
    let state = build_app_state(AppStateConfig {
        settings: Arc::clone(&shared_settings),
        registry_factory_override: None,
        max_concurrent_runs,
        store,
        artifact_store,
        vault_path,
        server_env_path,
        local_daemon_mode: true,
        env_lookup,
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
        auth_mode,
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
    let config_path_for_poll = config_path.clone();
    let args_for_poll = args.clone();
    let data_dir_for_poll = data_dir.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(5));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            match load_settings(config_path_for_poll.as_deref()) {
                Ok(new_disk_settings) => {
                    let effective = apply_runtime_settings(
                        &new_disk_settings,
                        &args_for_poll,
                        &data_dir_for_poll,
                    );
                    let changed = {
                        let cfg = state_for_poll
                            .settings
                            .read()
                            .expect("config lock poisoned");
                        *cfg != effective
                    };
                    if changed {
                        match state_for_poll.replace_settings(effective) {
                            Ok(()) => info!("Server config reloaded"),
                            Err(error) => warn!(
                                error = %error,
                                "Failed to resolve reloaded server config, keeping previous"
                            ),
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
    auth_codes: Arc<fabro_store::SlateAuthCodeStore>,
    auth_tokens: Arc<fabro_store::SlateAuthTokenStore>,
    shutdown_rx: watch::Receiver<bool>,
) {
    spawn_auth_code_reaper(auth_codes, shutdown_rx.clone());
    spawn_refresh_token_reaper(auth_tokens, shutdown_rx);
}

fn spawn_auth_code_reaper(
    auth_codes: Arc<fabro_store::SlateAuthCodeStore>,
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
    auth_tokens: Arc<fabro_store::SlateAuthTokenStore>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(6 * 60 * 60));
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

    use fabro_config::parse_settings_layer;
    use fabro_types::settings::SettingsLayer;
    use fabro_util::Home;

    use super::{
        GitHubMetaResolver, ServeArgs, ServerTitlePhase, apply_runtime_settings,
        bind_tcp_host_with_fallback, build_local_object_store_with_preference, build_slatedb_store,
        resolve_bind_request_from_settings, resolve_github_webhook_ip_allowlist,
        resolve_server_settings, resolve_startup_github_webhook_ip_allowlist, router_web_enabled,
        server_bind_title, server_title,
    };
    use crate::bind::{Bind, BindRequest};

    fn parse_settings(source: &str) -> SettingsLayer {
        parse_settings_layer(source).expect("v2 fixture should parse")
    }

    #[test]
    fn apply_runtime_settings_preserves_storage_dir() {
        let base = SettingsLayer::default();
        let args = ServeArgs {
            bind: None,
            model: None,
            provider: None,
            sandbox: None,
            web: false,
            no_web: false,
            max_concurrent_runs: None,
            config: None,
            #[cfg(debug_assertions)]
            watch_web: false,
        };

        let resolved = apply_runtime_settings(&base, &args, &PathBuf::from("/srv/fabro-storage"));

        let storage_root = resolved
            .server
            .as_ref()
            .and_then(|server| server.storage.as_ref())
            .and_then(|storage| storage.root.as_ref())
            .map(fabro_types::settings::InterpString::as_source);
        assert_eq!(storage_root.as_deref(), Some("/srv/fabro-storage"));
    }

    #[test]
    fn apply_runtime_settings_enables_web_from_cli_flag() {
        let base = parse_settings(
            r"
_version = 1

[server.web]
enabled = false
",
        );
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

        let resolved = apply_runtime_settings(&base, &args, &PathBuf::from("/srv/fabro"));

        assert_eq!(
            resolved
                .server
                .as_ref()
                .and_then(|server| server.web.as_ref())
                .and_then(|web| web.enabled),
            Some(true)
        );
    }

    #[test]
    fn apply_runtime_settings_disables_web_from_cli_flag() {
        let base = SettingsLayer::default();
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

        let resolved = apply_runtime_settings(&base, &args, &PathBuf::from("/srv/fabro"));

        assert_eq!(
            resolved
                .server
                .as_ref()
                .and_then(|server| server.web.as_ref())
                .and_then(|web| web.enabled),
            Some(false)
        );
    }

    #[test]
    fn resolve_bind_request_from_settings_defaults_to_socket_when_listen_is_absent() {
        let bind =
            resolve_bind_request_from_settings(&SettingsLayer::default(), None).expect("bind");

        assert_eq!(bind, BindRequest::Unix(Home::from_env().socket_path()));
    }

    #[test]
    fn resolve_bind_request_from_settings_uses_configured_tcp_when_no_explicit_bind_is_given() {
        let settings = parse_settings(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:0"
"#,
        );

        let bind = resolve_bind_request_from_settings(&settings, None).expect("bind");

        assert_eq!(bind, BindRequest::Tcp("127.0.0.1:0".parse().unwrap()));
    }

    #[test]
    fn resolve_bind_request_from_settings_prefers_explicit_bind_over_config() {
        let settings = parse_settings(
            r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"
"#,
        );

        let bind =
            resolve_bind_request_from_settings(&settings, Some("/tmp/fabro.sock")).expect("bind");

        assert_eq!(bind, BindRequest::Unix(PathBuf::from("/tmp/fabro.sock")));
    }

    #[test]
    fn resolve_bind_request_from_settings_preserves_host_only_cli_bind() {
        let settings = SettingsLayer::default();

        let bind = resolve_bind_request_from_settings(&settings, Some("127.0.0.1")).expect("bind");

        assert_eq!(bind, BindRequest::TcpHost("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn web_enabled_stays_enabled_without_github_app_mode() {
        let base = parse_settings(
            r#"
_version = 1

[server.web]
enabled = true

[server.integrations.github]
strategy = "token"
"#,
        );

        let resolved = resolve_server_settings(&base).expect("settings should resolve");

        assert!(router_web_enabled(&resolved));
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
        let settings = parse_settings(&format!(
            r#"
_version = 1

[server.slatedb.local]
root = "{}"
"#,
            root.display()
        ));

        let resolved = resolve_server_settings(&settings).expect("settings should resolve");
        let (_object_store, prefix, flush_interval, disk_cache) =
            build_slatedb_store(&resolved).expect("slatedb store should build");

        assert!(root.exists(), "configured SlateDB root should be created");
        assert_eq!(prefix, "");
        assert_eq!(flush_interval, Duration::from_millis(1));
        assert!(!disk_cache);
    }

    #[test]
    fn build_slatedb_store_returns_disk_cache_when_enabled() {
        let settings = parse_settings(
            r"
_version = 1

[server.slatedb]
disk_cache = true
",
        );

        let resolved = resolve_server_settings(&settings).expect("settings should resolve");
        let (_object_store, _prefix, _flush_interval, disk_cache) =
            build_slatedb_store(&resolved).expect("slatedb store should build");

        assert!(disk_cache);
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
        let settings = resolve_server_settings(&parse_settings(
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
        ))
        .expect("settings should resolve");

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
        let settings = resolve_server_settings(&parse_settings(
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
        ))
        .expect("settings should resolve");

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
