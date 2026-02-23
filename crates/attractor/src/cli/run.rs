use std::path::PathBuf;
use std::sync::Arc;

use anyhow::bail;
use chrono::Local;
use terminal::Styles;

use crate::checkpoint::Checkpoint;
use crate::engine::{PipelineEngine, RunConfig};
use crate::event::EventEmitter;
use crate::handler::default_registry;
use crate::interviewer::auto_approve::AutoApproveInterviewer;
use crate::interviewer::console::ConsoleInterviewer;
use crate::interviewer::Interviewer;
use crate::outcome::StageStatus;
use crate::pipeline::PipelineBuilder;
use crate::validation::Severity;

use super::backend::AgentBackend;
use super::{format_event_detail, format_event_summary, print_diagnostics, read_dot_file, RunArgs};

/// Execute a full pipeline run.
///
/// # Errors
///
/// Returns an error if the pipeline cannot be read, parsed, validated, or executed.
#[allow(clippy::too_many_lines)]
pub async fn run_command(args: RunArgs, styles: &'static Styles) -> anyhow::Result<()> {
    // 1. Parse and validate pipeline
    let source = read_dot_file(&args.pipeline)?;
    let (graph, diagnostics) = PipelineBuilder::new().prepare(&source)?;

    eprintln!(
        "{bold}Parsed pipeline:{reset} {} ({dim}{} nodes, {} edges{reset})",
        graph.name,
        graph.nodes.len(),
        graph.edges.len(),
        bold = styles.bold, dim = styles.dim, reset = styles.reset,
    );

    let goal = graph.goal();
    if !goal.is_empty() {
        eprintln!("{bold}Goal:{reset} {goal}", bold = styles.bold, reset = styles.reset);
    }

    print_diagnostics(&diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    // 2. Create logs directory
    let logs_dir = args.logs_dir.unwrap_or_else(|| {
        PathBuf::from(format!(
            "attractor-run-{}",
            Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    tokio::fs::create_dir_all(&logs_dir).await?;

    if args.verbose >= 1 {
        eprintln!(
            "{dim}Logs: {}{reset}",
            logs_dir.display(),
            dim = styles.dim, reset = styles.reset,
        );
    }

    // 3. Build event emitter
    let mut emitter = EventEmitter::new();
    if args.verbose >= 2 {
        emitter.on_event(move |event| {
            eprint!("{}", format_event_detail(event, styles));
        });
    } else if args.verbose >= 1 {
        emitter.on_event(move |event| {
            eprintln!("{}", format_event_summary(event, styles));
        });
    }

    // 4. Build interviewer
    let interviewer: Arc<dyn Interviewer> = if args.auto_approve {
        Arc::new(AutoApproveInterviewer)
    } else {
        Arc::new(ConsoleInterviewer::new(styles))
    };

    // 5. Resolve backend, model, and provider
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

    let provider = args.provider.or_else(|| {
        graph
            .attrs
            .get("default_provider")
            .and_then(|v| v.as_str())
            .map(String::from)
    });

    let model = args
        .model
        .or_else(|| {
            graph
                .attrs
                .get("default_model")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| match provider.as_deref() {
            Some("openai") => "gpt-5.2".to_string(),
            Some("gemini") => "gemini-3.1-pro-preview".to_string(),
            _ => "claude-opus-4-6".to_string(),
        });

    // 6. Build engine
    let registry = default_registry(interviewer.clone(), || {
        if dry_run_mode {
            None
        } else {
            Some(Box::new(AgentBackend::new(
                model.clone(),
                provider.clone(),
                args.verbose,
                styles,
                args.docker,
            )))
        }
    });
    let engine = PipelineEngine::with_interviewer(registry, emitter, interviewer);

    // 7. Execute
    let config = RunConfig {
        logs_root: logs_dir.clone(),
        cancel_token: None,
    };

    let outcome = if let Some(ref checkpoint_path) = args.resume {
        let checkpoint = Checkpoint::load(checkpoint_path)?;
        engine
            .run_from_checkpoint(&graph, &config, &checkpoint)
            .await?
    } else {
        engine.run(&graph, &config).await?
    };

    // 8. Print result
    eprintln!(
        "\n{bold}=== Pipeline Result ==={reset}",
        bold = styles.bold, reset = styles.reset,
    );

    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => styles.green,
        _ => styles.red,
    };
    eprintln!("Status: {status_color}{status_str}{reset}", reset = styles.reset);

    if let Some(notes) = &outcome.notes {
        eprintln!("Notes: {notes}");
    }
    if let Some(failure) = &outcome.failure_reason {
        eprintln!(
            "{red}Failure: {failure}{reset}",
            red = styles.red, reset = styles.reset,
        );
    }
    eprintln!(
        "{dim}Logs: {}{reset}",
        logs_dir.display(),
        dim = styles.dim, reset = styles.reset,
    );

    // 9. Exit code
    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => {
            std::process::exit(1);
        }
    }
}
