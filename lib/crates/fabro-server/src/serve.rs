use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use fabro_config::user::{active_settings_path, load_settings_config};
use fabro_config::{Storage, resolve_server_from_file};
use fabro_llm::client::Client as LlmClient;
use fabro_sandbox::SandboxProvider;
use fabro_types::settings::{
    InterpString, ObjectStoreSettings, ServerListenSettings,
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
use crate::github_webhooks::WebhookManager;
use crate::jwt_auth::{AuthMode, AuthStrategy, resolve_auth_mode_with_lookup};
use crate::secret_store::SecretStore;
use crate::server::{
    RouterOptions, build_app_state_with_path, build_router_with_options,
    reconcile_incomplete_runs_on_startup, shutdown_active_workers, spawn_scheduler,
};
use crate::tls::{ClientAuth, build_rustls_config, serve_tls_with_shutdown};

const TEST_IN_MEMORY_STORE_ENV: &str = "FABRO_TEST_IN_MEMORY_STORE";
pub const DEFAULT_TCP_PORT: u16 = 32276;

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

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub dry_run: bool,

    /// Sandbox for agent tools
    #[arg(long, value_enum)]
    pub sandbox: Option<SandboxProvider>,

    /// Maximum number of concurrent run executions
    #[arg(long)]
    pub max_concurrent_runs: Option<usize>,

    /// Path to server config file (default: ~/.fabro/settings.toml)
    #[arg(long)]
    pub config: Option<PathBuf>,
}

fn load_settings(path: Option<&Path>) -> anyhow::Result<SettingsLayer> {
    load_settings_config(path)
}

fn resolved_config_path(path: Option<&Path>) -> PathBuf {
    active_settings_path(path)
}

fn apply_serve_overrides(
    base: &SettingsLayer,
    args: &ServeArgs,
    dry_run_mode: bool,
) -> SettingsLayer {
    use fabro_types::settings::cli::CliLayer;
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::run::{
        RunExecutionLayer, RunLayer, RunMode, RunModelLayer, RunSandboxLayer,
    };
    use fabro_types::settings::server::{ServerLayer, ServerWebLayer};
    let mut settings = base.clone();
    if dry_run_mode {
        let run = settings.run.get_or_insert_with(RunLayer::default);
        let execution = run.execution.get_or_insert_with(RunExecutionLayer::default);
        execution.mode = Some(RunMode::DryRun);
    }
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
    dry_run_mode: bool,
    data_dir: &Path,
) -> SettingsLayer {
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::server::{ServerLayer, ServerStorageLayer};
    let mut settings = apply_serve_overrides(base, args, dry_run_mode);
    let server = settings.server.get_or_insert_with(ServerLayer::default);
    let storage = server
        .storage
        .get_or_insert_with(ServerStorageLayer::default);
    storage.root = Some(InterpString::parse(&data_dir.to_string_lossy()));
    settings
}

fn use_in_memory_store() -> bool {
    !matches!(
        std::env::var(TEST_IN_MEMORY_STORE_ENV).ok().as_deref(),
        None | Some("" | "0" | "false" | "no")
    )
}

fn build_object_store_with_preference(
    store_path: &Path,
    use_in_memory: bool,
) -> anyhow::Result<Arc<dyn ObjectStore>> {
    if use_in_memory {
        return Ok(Arc::new(InMemory::new()));
    }

    std::fs::create_dir_all(store_path)?;
    Ok(Arc::new(LocalFileSystem::new_with_prefix(store_path)?))
}

fn build_object_store(store_path: &Path) -> anyhow::Result<Arc<dyn ObjectStore>> {
    build_object_store_with_preference(store_path, use_in_memory_store())
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

fn resolve_interp(value: &InterpString) -> anyhow::Result<String> {
    value
        .resolve(|name| std::env::var(name).ok())
        .map(|resolved| resolved.value)
        .with_context(|| format!("failed to resolve {}", value.as_source()))
}

fn resolve_interp_path(value: &InterpString) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(resolve_interp(value)?))
}

fn build_artifact_object_store(
    settings: &ResolvedServerSettings,
) -> anyhow::Result<(Arc<dyn ObjectStore>, String)> {
    let prefix = resolve_interp(&settings.artifacts.prefix)?;

    if use_in_memory_store() {
        return Ok((Arc::new(InMemory::new()), prefix));
    }

    match &settings.artifacts.store {
        ObjectStoreSettings::Local { root } => {
            let root = resolve_interp_path(root)?;
            std::fs::create_dir_all(&root)?;
            let object_store = Arc::new(LocalFileSystem::new_with_prefix(&root)?);
            Ok((object_store, prefix))
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
            let object_store = Arc::new(builder.build()?);
            Ok((object_store, prefix))
        }
    }
}

/// Start the HTTP API server.
///
/// # Errors
///
/// Returns an error if the server fails to bind or encounters a fatal error.
#[allow(clippy::print_stderr)]
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

    let config_path = args.config.clone();
    let disk_settings = load_settings(config_path.as_deref())?;
    let disk_server_settings = resolve_server_settings(&disk_settings)?;
    let active_config_path = resolved_config_path(config_path.as_deref());
    let data_dir = match storage_dir_override {
        Some(path) => path,
        None => resolve_interp_path(&disk_server_settings.storage.root)?,
    };
    let storage = Storage::new(&data_dir);
    let secret_store_path = storage.secrets_path();
    let secret_store = SecretStore::load(secret_store_path.clone())?;
    let secret_snapshot = secret_store.snapshot();

    // Resolve dry-run mode (same pattern as run.rs)
    let dry_run_mode = if args.dry_run {
        true
    } else {
        match LlmClient::from_lookup(|name| {
            secret_snapshot
                .get(name)
                .cloned()
                .or_else(|| std::env::var(name).ok())
        })
        .await
        {
            Ok(c) if c.provider_names().is_empty() => {
                eprintln!(
                    "{} No LLM providers configured. Running in dry-run mode.",
                    styles.yellow.apply_to("Warning:"),
                );
                true
            }
            Ok(_) => false,
            Err(e) => {
                eprintln!(
                    "{} Failed to initialize LLM client: {e}. Running in dry-run mode.",
                    styles.yellow.apply_to("Warning:"),
                );
                true
            }
        }
    };

    // Shared config for live reloading
    let effective_settings = apply_runtime_settings(&disk_settings, &args, dry_run_mode, &data_dir);
    let resolved_server_settings = resolve_server_settings(&effective_settings)?;
    let shared_settings = Arc::new(RwLock::new(effective_settings));
    std::fs::create_dir_all(&data_dir)?;
    let (auth_mode, client_auth, max_concurrent_runs) = {
        let auth_mode = resolve_auth_mode_with_lookup(&resolved_server_settings, |name| {
            secret_snapshot
                .get(name)
                .cloned()
                .or_else(|| std::env::var(name).ok())
        })?;
        let tls_present = matches!(
            resolved_server_settings.listen,
            ServerListenSettings::Tcp { ref tls, .. } if tls.is_some()
        );
        let client_auth = tls_present.then(|| client_auth_from_mode(&auth_mode));
        let max_concurrent_runs = args
            .max_concurrent_runs
            .unwrap_or(resolved_server_settings.scheduler.max_concurrent_runs);
        (auth_mode, client_auth, max_concurrent_runs)
    };
    let web_enabled = resolved_server_settings.web.enabled;

    let store_path = storage.store_dir();
    let object_store = build_object_store(&store_path)?;
    let store = Arc::new(fabro_store::Database::new(
        Arc::clone(&object_store),
        "",
        Duration::from_millis(1),
    ));
    let (artifact_object_store, artifact_prefix) =
        build_artifact_object_store(&resolved_server_settings)?;
    let artifact_store = fabro_store::ArtifactStore::new(artifact_object_store, artifact_prefix);
    let state = build_app_state_with_path(
        Arc::clone(&shared_settings),
        None,
        max_concurrent_runs,
        store,
        artifact_store,
        secret_store_path,
        active_config_path,
        matches!(&auth_mode, AuthMode::Disabled),
    )?;
    let reconciled = reconcile_incomplete_runs_on_startup(&state).await?;
    if reconciled > 0 {
        info!(
            reconciled_runs = reconciled,
            "Reconciled stale in-flight runs on startup"
        );
    }
    spawn_scheduler(Arc::clone(&state));
    let router =
        build_router_with_options(Arc::clone(&state), auth_mode, RouterOptions { web_enabled });

    let bind_request = match args.bind {
        Some(ref s) => bind::parse_bind(s)?,
        None => match &resolved_server_settings.listen {
            ServerListenSettings::Unix { path } => BindRequest::Unix(resolve_interp_path(path)?),
            ServerListenSettings::Tcp { address, .. } => BindRequest::Tcp(*address),
        },
    };

    // Optionally start webhook listener
    let webhook_app_id = resolved_server_settings
        .integrations
        .github
        .webhooks
        .as_ref()
        .and(resolved_server_settings.integrations.github.app_id.as_ref())
        .map(resolve_interp)
        .transpose()?;
    let webhook_manager = match webhook_app_id {
        Some(app_id) => {
            let secret = secret_snapshot
                .get("GITHUB_APP_WEBHOOK_SECRET")
                .cloned()
                .or_else(|| std::env::var("GITHUB_APP_WEBHOOK_SECRET").ok());
            let github_app = state
                .github_app_credentials(Some(&app_id))
                .await
                .unwrap_or_else(|err| {
                    warn!(
                        error = %err,
                        "Webhook config present but GITHUB_APP_PRIVATE_KEY is invalid; skipping webhook listener"
                    );
                    None
                });
            if let (Some(secret), Some(github_app)) = (secret, github_app) {
                match WebhookManager::start(
                    secret.into_bytes(),
                    &github_app.app_id,
                    &github_app.private_key_pem,
                )
                .await
                {
                    Ok(manager) => Some(manager),
                    Err(err) => {
                        error!(error = %err, "Failed to start webhook listener");
                        None
                    }
                }
            } else {
                warn!(
                    "Webhook config present but GITHUB_APP_WEBHOOK_SECRET or GITHUB_APP_PRIVATE_KEY not set; skipping webhook listener"
                );
                None
            }
        }
        None => None,
    };

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
                        dry_run_mode,
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

    // Branch: TLS, plain TCP, or Unix socket
    let tls_settings = match &resolved_server_settings.listen {
        ServerListenSettings::Tcp { tls, .. } => tls.clone(),
        ServerListenSettings::Unix { .. } => None,
    };

    let bound_listener = bind_listener(&bind_request).await?;
    let bind_addr = bound_listener.bind.clone();
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

    match bound_listener.listener {
        BoundListener::Unix(listener) => {
            if tls_settings.is_some() {
                warn!("TLS is configured but not supported on Unix sockets; ignoring TLS settings");
            }
            announce_server_ready(&bind_addr, styles, dry_run_mode);
            axum::serve(listener, router)
                .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()))
                .await?;
        }
        BoundListener::Tcp(listener) => {
            if let Some(ref tls_settings) = tls_settings {
                let client_auth = client_auth.unwrap();
                let rustls_config = build_rustls_config(tls_settings, client_auth)?;
                let tls_acceptor = tokio_rustls::TlsAcceptor::from(rustls_config);

                info!("TLS enabled");
                announce_server_ready(&bind_addr, styles, dry_run_mode);

                serve_tls_with_shutdown(
                    listener,
                    tls_acceptor,
                    router,
                    wait_for_shutdown(shutdown_rx.clone()),
                )
                .await?;
            } else {
                announce_server_ready(&bind_addr, styles, dry_run_mode);
                axum::serve(listener, router)
                    .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()))
                    .await?;
            }
        }
    }

    // Clean up webhook listener on shutdown
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
                std::fs::remove_file(path)?;
            }

            let listener = UnixListener::bind(path)?;
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

#[allow(clippy::print_stderr)] // Startup status belongs on stderr for operator-facing CLI output.
fn announce_server_ready(bind_addr: &Bind, styles: &'static Styles, dry_run_mode: bool) {
    set_server_title(ServerTitlePhase::Listening, Some(bind_addr));
    info!(bind = %bind_addr, dry_run = dry_run_mode, "API server started");

    eprintln!(
        "{}",
        styles.bold.apply_to(format!(
            "Fabro server listening on {}",
            styles.cyan.apply_to(bind_addr)
        )),
    );
    if dry_run_mode {
        eprintln!("{}", styles.dim.apply_to("(dry-run mode)"));
    }
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

/// Derive client certificate verification mode from the resolved auth
/// strategies.
fn client_auth_from_mode(auth_mode: &AuthMode) -> ClientAuth {
    let strategies = match auth_mode {
        AuthMode::Strategies(s) => s,
        AuthMode::Disabled => return ClientAuth::None,
    };

    let has_mtls = strategies.iter().any(|s| matches!(s, AuthStrategy::Mtls));
    if !has_mtls {
        return ClientAuth::None;
    }

    if strategies.len() > 1 {
        ClientAuth::Optional
    } else {
        ClientAuth::Required
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_config::parse_settings_layer;
    use fabro_types::settings::SettingsLayer;

    use super::{
        ServeArgs, ServerTitlePhase, apply_runtime_settings, bind_tcp_host_with_fallback,
        build_object_store_with_preference, server_bind_title, server_title,
    };
    use crate::bind::Bind;

    fn parse_settings(source: &str) -> SettingsLayer {
        parse_settings_layer(source).expect("v2 fixture should parse")
    }

    #[test]
    fn apply_runtime_settings_preserves_storage_dir() {
        let base = SettingsLayer::default();
        let args = ServeArgs {
            bind:                None,
            model:               None,
            provider:            None,
            dry_run:             false,
            sandbox:             None,
            web:                 false,
            no_web:              false,
            max_concurrent_runs: None,
            config:              None,
        };

        let resolved =
            apply_runtime_settings(&base, &args, false, &PathBuf::from("/srv/fabro-storage"));

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
            bind:                None,
            model:               None,
            provider:            None,
            dry_run:             false,
            sandbox:             None,
            web:                 true,
            no_web:              false,
            max_concurrent_runs: None,
            config:              None,
        };

        let resolved = apply_runtime_settings(&base, &args, false, &PathBuf::from("/srv/fabro"));

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
            bind:                None,
            model:               None,
            provider:            None,
            dry_run:             false,
            sandbox:             None,
            web:                 false,
            no_web:              true,
            max_concurrent_runs: None,
            config:              None,
        };

        let resolved = apply_runtime_settings(&base, &args, false, &PathBuf::from("/srv/fabro"));

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

        let disk_store = build_object_store_with_preference(&store_path, false)
            .expect("disk-backed store should build");
        assert!(
            store_path.exists(),
            "disk-backed store should create store dir"
        );
        drop(disk_store);

        let mem_path = temp.path().join("memory-store");
        let mem_store = build_object_store_with_preference(&mem_path, true)
            .expect("memory-backed store should build");
        assert!(
            !mem_path.exists(),
            "memory-backed store should not create on-disk store dir"
        );
        drop(mem_store);
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
