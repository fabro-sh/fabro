use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use fabro_config::server::{load_server_settings, resolve_storage_dir};
use fabro_util::terminal::Styles;
use object_store::local::LocalFileSystem;
use tokio::net::{TcpListener, UnixListener};
use tokio::time::interval;
use tracing::{error, info, warn};

use clap::Args;

use fabro_types::Settings;

use crate::bind::{self, Bind};
use crate::github_webhooks::WebhookManager;
use crate::jwt_auth::{AuthMode, AuthStrategy, resolve_auth_mode};
use crate::server::{build_router, create_app_state_with_store, spawn_scheduler};
use crate::tls::{ClientAuth, build_rustls_config, serve_tls};
use fabro_llm::client::Client as LlmClient;
use fabro_sandbox::SandboxProvider;

#[derive(Args, Clone)]
pub struct ServeArgs {
    /// Address to bind to (host:port for TCP, or path containing / for Unix socket)
    #[arg(long)]
    pub bind: Option<String>,

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

    /// Path to server config file (default: ~/.fabro/server.toml)
    #[arg(long)]
    pub config: Option<PathBuf>,
}

fn apply_serve_overrides(base: &Settings, args: &ServeArgs, dry_run_mode: bool) -> Settings {
    let mut settings = base.clone();
    if dry_run_mode {
        settings.dry_run = Some(true);
    }
    if let Some(ref model) = args.model {
        settings.llm.get_or_insert_default().model = Some(model.clone());
    }
    if let Some(ref provider) = args.provider {
        settings.llm.get_or_insert_default().provider = Some(provider.clone());
    }
    if let Some(sandbox) = args.sandbox {
        settings.sandbox.get_or_insert_default().provider = Some(sandbox.to_string());
    }
    settings
}

/// Start the HTTP API server.
///
/// # Errors
///
/// Returns an error if the server fails to bind or encounters a fatal error.
#[allow(clippy::print_stderr)]
pub async fn serve_command(
    args: ServeArgs,
    styles: &'static Styles,
    storage_dir_override: Option<PathBuf>,
) -> anyhow::Result<()> {
    // Resolve dry-run mode (same pattern as run.rs)
    let dry_run_mode = if args.dry_run {
        true
    } else {
        match LlmClient::from_env().await {
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

    // Initialize data directory and storage
    let config_path = args.config.clone();
    let disk_settings = load_server_settings(config_path.as_deref())?;
    let data_dir = storage_dir_override.unwrap_or_else(|| resolve_storage_dir(&disk_settings));

    // Shared config for live reloading
    let shared_settings = Arc::new(RwLock::new(apply_serve_overrides(
        &disk_settings,
        &args,
        dry_run_mode,
    )));
    std::fs::create_dir_all(&data_dir)?;
    let (auth_mode, client_auth, max_concurrent_runs) = {
        let cfg = shared_settings.read().expect("config lock poisoned");
        let api = cfg.api.clone().unwrap_or_default();
        let allowed_usernames = cfg
            .web
            .as_ref()
            .map(|w| w.auth.allowed_usernames.clone())
            .unwrap_or_default();
        let auth_mode = resolve_auth_mode(&api, &allowed_usernames);
        let client_auth = api.tls.as_ref().map(|_| client_auth_from_mode(&auth_mode));
        let max_concurrent_runs = args
            .max_concurrent_runs
            .or(cfg.max_concurrent_runs)
            .unwrap_or(5);
        (auth_mode, client_auth, max_concurrent_runs)
    };

    let store_path = data_dir.join("store");
    std::fs::create_dir_all(&store_path)?;
    let object_store = Arc::new(LocalFileSystem::new_with_prefix(&store_path)?);
    let store = Arc::new(fabro_store::SlateStore::new(
        object_store,
        "",
        Duration::from_millis(1),
    ));
    let state =
        create_app_state_with_store(Arc::clone(&shared_settings), max_concurrent_runs, store);
    spawn_scheduler(Arc::clone(&state));
    let router = build_router(state, auth_mode);

    let bind_addr = match args.bind {
        Some(ref s) => bind::parse_bind(s)?,
        None => Bind::Tcp("127.0.0.1:3000".parse().unwrap()),
    };

    info!(bind = %bind_addr, dry_run = dry_run_mode, "API server started");

    eprintln!(
        "{}",
        styles.bold.apply_to(format!(
            "Fabro server listening on {}",
            styles.cyan.apply_to(&bind_addr)
        )),
    );
    if dry_run_mode {
        eprintln!("{}", styles.dim.apply_to("(dry-run mode)"));
    }

    // Optionally start webhook listener
    let webhook_app_id = {
        let cfg = shared_settings.read().expect("config lock poisoned");
        cfg.git
            .as_ref()
            .and_then(|g| g.webhooks.as_ref().and(g.app_id.as_ref()))
            .cloned()
    };
    let webhook_manager = match webhook_app_id {
        Some(app_id) => {
            let secret = std::env::var("GITHUB_APP_WEBHOOK_SECRET").ok();
            let github_app = match fabro_github::GitHubAppCredentials::from_env(Some(&app_id)) {
                Ok(github_app) => github_app,
                Err(err) => {
                    warn!(
                        error = %err,
                        "Webhook config present but GITHUB_APP_PRIVATE_KEY is invalid; skipping webhook listener"
                    );
                    None
                }
            };
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

    // Spawn config polling task
    let settings_for_poll = Arc::clone(&shared_settings);
    let config_path_for_poll = config_path.clone();
    let args_for_poll = args.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(5));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            match load_server_settings(config_path_for_poll.as_deref()) {
                Ok(new_disk_settings) => {
                    let effective =
                        apply_serve_overrides(&new_disk_settings, &args_for_poll, dry_run_mode);
                    let changed = {
                        let cfg = settings_for_poll.read().expect("config lock poisoned");
                        *cfg != effective
                    };
                    if changed {
                        let mut cfg = settings_for_poll.write().expect("config lock poisoned");
                        *cfg = effective;
                        info!("Server config reloaded");
                    }
                }
                Err(e) => {
                    warn!("Failed to reload server config, keeping previous: {e}");
                }
            }
        }
    });

    // Branch: TLS, plain TCP, or Unix socket
    let tls_settings = shared_settings
        .read()
        .expect("config lock poisoned")
        .api
        .as_ref()
        .and_then(|a| a.tls.clone());

    match bind_addr {
        Bind::Unix(ref path) => {
            if tls_settings.is_some() {
                warn!("TLS is configured but not supported on Unix sockets; ignoring TLS settings");
            }

            // Remove stale socket file before binding
            if path.exists() {
                std::fs::remove_file(path)?;
            }

            let listener = UnixListener::bind(path)?;
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
        Bind::Tcp(addr) => {
            let listener = TcpListener::bind(addr).await?;

            if let Some(ref tls_settings) = tls_settings {
                let client_auth = client_auth.unwrap();
                let rustls_config = build_rustls_config(tls_settings, client_auth);
                let tls_acceptor = tokio_rustls::TlsAcceptor::from(rustls_config);

                info!("TLS enabled");

                // TLS uses a manual accept loop and cannot use with_graceful_shutdown
                serve_tls(listener, tls_acceptor, router).await?;
            } else {
                axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown_signal())
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

/// Derive client certificate verification mode from the resolved auth strategies.
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
