use std::sync::Arc;

use terminal::Styles;
use tokio::net::TcpListener;

use crate::cli::backend::AgentBackend;
use crate::handler::default_registry;
use crate::interviewer::Interviewer;
use crate::server::{build_router, create_app_state_with_options};

use super::ServeArgs;

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
        match llm::client::Client::from_env().await {
            Ok(c) if c.provider_names().is_empty() => {
                eprintln!(
                    "{yellow}Warning:{reset} No LLM providers configured. Running in dry-run mode.",
                    yellow = styles.yellow, reset = styles.reset,
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
    let provider = args.provider;
    let model = args.model.unwrap_or_else(|| match provider.as_deref() {
        Some("openai") => "gpt-5.2".to_string(),
        Some("gemini") => "gemini-3.1-pro-preview".to_string(),
        _ => "claude-opus-4-6".to_string(),
    });

    // Resolve model alias through catalog
    let (model, provider) = match llm::catalog::get_model_info(&model) {
        Some(info) => (info.id, provider.or(Some(info.provider))),
        None => (model, provider),
    };

    // Build registry factory
    let factory = move |interviewer: Arc<dyn Interviewer>| {
        let model = model.clone();
        let provider = provider.clone();
        default_registry(interviewer, move || {
            if dry_run_mode {
                None
            } else {
                Some(Box::new(AgentBackend::new(
                    model.clone(),
                    provider.clone(),
                    0,
                    styles,
                    args.docker,
                )))
            }
        })
    };

    let state = create_app_state_with_options(factory, dry_run_mode);
    let router = build_router(state);

    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr).await?;

    eprintln!(
        "{bold}Attractor server listening on {green}{addr}{reset}",
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
