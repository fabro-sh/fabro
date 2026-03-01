use std::sync::Arc;

use arc_llm::provider::Provider;
use arc_util::terminal::Styles;
use tokio::net::TcpListener;

use clap::Args;

use crate::server::{build_router, create_app_state_with_options};
use arc_workflows::cli::backend::AgentBackend;
use arc_workflows::cli::ExecutionEnvKind;
use arc_workflows::handler::default_registry;
use arc_workflows::interviewer::Interviewer;

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

    /// Execution environment for agent tools
    #[arg(long, value_enum)]
    pub execution_env: Option<ExecutionEnvKind>,
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
        match arc_llm::client::Client::from_env().await {
            Ok(c) if c.provider_names().is_empty() => {
                eprintln!(
                    "{yellow}Warning:{reset} No LLM providers configured. Running in dry-run mode.",
                    yellow = styles.yellow,
                    reset = styles.reset,
                );
                true
            }
            Ok(_) => false,
            Err(e) => {
                eprintln!(
                    "{yellow}Warning:{reset} Failed to initialize LLM client: {e}. Running in dry-run mode.",
                    yellow = styles.yellow, reset = styles.reset,
                );
                true
            }
        }
    };

    // Resolve model/provider defaults
    let provider_str = args.provider;
    let model = args.model.unwrap_or_else(|| match provider_str.as_deref() {
        Some("openai") => "gpt-5.2".to_string(),
        Some("gemini") => "gemini-3.1-pro-preview".to_string(),
        _ => "claude-opus-4-6".to_string(),
    });

    // Resolve model alias through catalog
    let (model, provider_str) = match arc_llm::catalog::get_model_info(&model) {
        Some(info) => (info.id, provider_str.or(Some(info.provider))),
        None => (model, provider_str),
    };

    // Parse provider string to enum (defaults to Anthropic)
    let provider_enum: Provider = provider_str
        .as_deref()
        .map(|s| s.parse::<Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or(Provider::Anthropic);

    // Build registry factory
    let factory = move |interviewer: Arc<dyn Interviewer>| {
        let model = model.clone();
        default_registry(interviewer, move || {
            if dry_run_mode {
                None
            } else {
                Some(Box::new(AgentBackend::new(
                    model.clone(),
                    provider_enum,
                    0,
                    styles,
                )))
            }
        })
    };

    let auth_mode = crate::jwt_auth::resolve_auth_mode();

    let state = create_app_state_with_options(factory, dry_run_mode);
    let router = build_router(state, auth_mode);

    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr).await?;

    eprintln!(
        "{bold}Arc server listening on {green}{addr}{reset}",
        bold = styles.bold,
        green = styles.green,
        reset = styles.reset,
    );
    if dry_run_mode {
        eprintln!(
            "{dim}(dry-run mode){reset}",
            dim = styles.dim,
            reset = styles.reset,
        );
    }

    axum::serve(listener, router).await?;

    Ok(())
}
