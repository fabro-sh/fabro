use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use fabro_model::Provider;
use fabro_util::terminal::Styles;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use clap::Args;

use fabro_config::server::ServerConfig;

use crate::jwt_auth::{AuthMode, AuthStrategy};
use crate::server::build_router;
use crate::tls::ClientAuth;
use fabro_interview::Interviewer;
use fabro_workflows::backend::AgentApiBackend;
use fabro_workflows::handler::default_registry;
use fabro_workflows::sandbox_provider::SandboxProvider;

#[derive(Args)]
pub struct ServeArgs {
    /// Port to listen on
    #[arg(long, default_value = "3000")]
    pub port: u16,

    /// Host address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

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

/// Start the HTTP API server.
///
/// # Errors
///
/// Returns an error if the server fails to bind or encounters a fatal error.
pub async fn serve_command(args: ServeArgs, styles: &'static Styles) -> anyhow::Result<()> {
    // Resolve dry-run mode (same pattern as run.rs)
    let dry_run_mode = if args.dry_run {
        true
    } else {
        match fabro_llm::client::Client::from_env().await {
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

    // Initialize data directory and SQLite database
    let config_path = args.config;
    let server_config = fabro_config::server::load_server_config(config_path.as_deref())?;
    let data_dir = fabro_config::server::resolve_data_dir(&server_config);

    // Shared config for live reloading
    let shared_config = Arc::new(RwLock::new(server_config));

    // CLI overrides take precedence over config file values, even after reload
    let cli_model = args.model;
    let cli_provider = args.provider;

    // Build registry factory that reads live config
    let config_for_factory = Arc::clone(&shared_config);
    let factory = move |interviewer: Arc<dyn Interviewer>| {
        let (model, provider_enum) = resolve_model_provider(
            &config_for_factory,
            cli_model.as_deref(),
            cli_provider.as_deref(),
        );
        default_registry(interviewer, move || {
            if dry_run_mode {
                None
            } else {
                Some(Box::new(AgentApiBackend::new(
                    model.clone(),
                    provider_enum,
                    Vec::new(),
                )))
            }
        })
    };
    std::fs::create_dir_all(&data_dir)?;
    let db = fabro_db::connect(&data_dir.join("fabro.db")).await?;
    fabro_db::initialize_db(&db).await?;

    let (auth_mode, client_auth, max_concurrent_runs) = {
        let cfg = shared_config.read().expect("config lock poisoned");
        let auth_mode =
            crate::jwt_auth::resolve_auth_mode(&cfg.api, cfg.web.auth.allowed_usernames.clone());
        let client_auth = cfg
            .api
            .tls
            .as_ref()
            .map(|_| client_auth_from_mode(&auth_mode));
        let max_concurrent_runs = args
            .max_concurrent_runs
            .or(cfg.max_concurrent_runs)
            .unwrap_or(5);
        (auth_mode, client_auth, max_concurrent_runs)
    };

    let git_author = {
        let cfg = shared_config.read().expect("config lock poisoned");
        fabro_workflows::git::GitAuthor::from_options(
            cfg.git.author.name.clone(),
            cfg.git.author.email.clone(),
        )
    };
    let hooks = {
        let cfg = shared_config.read().expect("config lock poisoned");
        cfg.run_defaults.hooks.clone()
    };
    let state = crate::server::create_app_state_with_options(
        db,
        factory,
        dry_run_mode,
        max_concurrent_runs,
        git_author,
        hooks,
    );
    crate::server::spawn_scheduler(Arc::clone(&state));
    let router = build_router(state, auth_mode);

    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr).await?;

    info!(host = %args.host, port = args.port, dry_run = dry_run_mode, "API server started");

    eprintln!(
        "{}",
        styles.bold.apply_to(format!(
            "Fabro server listening on {}",
            styles.cyan.apply_to(&addr)
        )),
    );
    if dry_run_mode {
        eprintln!("{}", styles.dim.apply_to("(dry-run mode)"));
    }

    // Optionally start webhook listener
    let webhook_app_id = {
        let cfg = shared_config.read().expect("config lock poisoned");
        match (&cfg.git.webhooks, &cfg.git.app_id) {
            (Some(_), Some(app_id)) => Some(app_id.clone()),
            _ => None,
        }
    };
    let webhook_manager = match webhook_app_id {
        Some(app_id) => {
            let secret = std::env::var("GITHUB_APP_WEBHOOK_SECRET").ok();
            let private_key_pem = read_github_private_key();
            match (secret, private_key_pem) {
                (Some(secret), Some(pem)) => {
                    match crate::github_webhooks::WebhookManager::start(
                        secret.into_bytes(),
                        &app_id,
                        &pem,
                    )
                    .await
                    {
                        Ok(manager) => Some(manager),
                        Err(err) => {
                            error!(error = %err, "Failed to start webhook listener");
                            None
                        }
                    }
                }
                _ => {
                    warn!("Webhook config present but GITHUB_APP_WEBHOOK_SECRET or GITHUB_APP_PRIVATE_KEY not set; skipping webhook listener");
                    None
                }
            }
        }
        None => None,
    };

    // Spawn config polling task
    let config_for_poll = Arc::clone(&shared_config);
    let config_path_for_poll = config_path.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            match fabro_config::server::load_server_config(config_path_for_poll.as_deref()) {
                Ok(new_config) => {
                    let changed = {
                        let cfg = config_for_poll.read().expect("config lock poisoned");
                        *cfg != new_config
                    };
                    if changed {
                        let mut cfg = config_for_poll.write().expect("config lock poisoned");
                        *cfg = new_config;
                        info!("Server config reloaded");
                    }
                }
                Err(e) => {
                    warn!("Failed to reload server config, keeping previous: {e}");
                }
            }
        }
    });

    // Branch: TLS or plain HTTP
    let tls_config = shared_config
        .read()
        .expect("config lock poisoned")
        .api
        .tls
        .clone();
    if let Some(ref tls_config) = tls_config {
        let client_auth = client_auth.unwrap();

        let rustls_config = crate::tls::build_rustls_config(tls_config, client_auth);
        let tls_acceptor = tokio_rustls::TlsAcceptor::from(rustls_config);

        info!("TLS enabled");

        crate::tls::serve_tls(listener, tls_acceptor, router).await?;
    } else {
        axum::serve(listener, router).await?;
    }

    // Clean up webhook listener on shutdown
    if let Some(manager) = webhook_manager {
        manager.shutdown().await;
    }

    Ok(())
}

/// Resolve model and provider from shared config, with CLI overrides taking precedence.
fn resolve_model_provider(
    shared_config: &RwLock<ServerConfig>,
    cli_model: Option<&str>,
    cli_provider: Option<&str>,
) -> (String, Provider) {
    let cfg = shared_config.read().expect("config lock poisoned");
    let config_provider = cfg
        .run_defaults
        .llm
        .as_ref()
        .and_then(|l| l.provider.as_deref());
    let config_model = cfg
        .run_defaults
        .llm
        .as_ref()
        .and_then(|l| l.model.as_deref());

    let provider_str = cli_provider.or(config_provider);
    let model = cli_model
        .map(|s| s.to_string())
        .or_else(|| config_model.map(|s| s.to_string()))
        .unwrap_or_else(|| {
            // Look up default model from catalog for the given provider,
            // falling back to the best provider with an API key configured.
            provider_str
                .and_then(fabro_model::default_model_for_provider)
                .unwrap_or_else(fabro_model::default_model_from_env)
                .id
        });

    // Resolve model alias through catalog
    let (model, provider_str) = match fabro_model::get_model_info(&model) {
        Some(info) => (
            info.id,
            provider_str.map(|s| s.to_string()).or(Some(info.provider)),
        ),
        None => (model, provider_str.map(|s| s.to_string())),
    };

    let provider_enum: Provider = provider_str
        .as_deref()
        .and_then(|s| s.parse::<Provider>().ok())
        .unwrap_or_else(Provider::default_from_env);

    (model, provider_enum)
}

/// Read the GitHub App private key from the environment, decoding base64 if needed.
fn read_github_private_key() -> Option<String> {
    let raw = std::env::var("GITHUB_APP_PRIVATE_KEY").ok()?;
    Some(crate::jwt_auth::decode_pem_env(
        "GITHUB_APP_PRIVATE_KEY",
        &raw,
    ))
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
