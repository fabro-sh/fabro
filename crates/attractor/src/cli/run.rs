use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::bail;
use chrono::{Local, Utc};
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
use super::{compute_stage_cost, format_cost, format_duration_human, format_event_detail, format_event_summary, format_tokens_human, print_diagnostics, read_dot_file, RunArgs};

/// Accumulates token usage and cost across all pipeline stages.
#[derive(Default)]
struct CostAccumulator {
    total_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_read_tokens: i64,
    total_cache_write_tokens: i64,
    total_reasoning_tokens: i64,
    total_cost: f64,
    has_pricing: bool,
}

/// Execute a full pipeline run.
///
/// # Errors
///
/// Returns an error if the pipeline cannot be read, parsed, validated, or executed.
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
        let base = dirs::home_dir()
            .expect("could not determine home directory")
            .join(".attractor")
            .join("logs");
        base.join(format!(
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

    // Cost accumulator — shared across all verbosity levels
    let accumulator = Arc::new(Mutex::new(CostAccumulator::default()));
    let acc_clone = Arc::clone(&accumulator);
    emitter.on_event(move |event| {
        if let crate::event::PipelineEvent::StageCompleted { usage: Some(u), .. } = event {
            let mut acc = acc_clone.lock().unwrap();
            acc.total_input_tokens += u.input_tokens;
            acc.total_output_tokens += u.output_tokens;
            acc.total_cache_read_tokens += u.cache_read_tokens.unwrap_or(0);
            acc.total_cache_write_tokens += u.cache_write_tokens.unwrap_or(0);
            acc.total_reasoning_tokens += u.reasoning_tokens.unwrap_or(0);
            if let Some(cost) = compute_stage_cost(u) {
                acc.total_cost += cost;
                acc.has_pricing = true;
            }
        }
    });

    // NDJSON progress log + live.json snapshot
    {
        let ndjson_path = logs_dir.join("progress.ndjson");
        let live_path = logs_dir.join("live.json");
        let run_id = Arc::new(Mutex::new(String::new()));
        let run_id_clone = Arc::clone(&run_id);
        emitter.on_event(move |event| {
            if let crate::event::PipelineEvent::PipelineStarted { id, .. } = event {
                *run_id_clone.lock().unwrap() = id.clone();
            }
            let envelope = serde_json::json!({
                "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "run_id": *run_id_clone.lock().unwrap(),
                "event": event,
            });
            // Append to progress.ndjson
            if let Ok(line) = serde_json::to_string(&envelope) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&ndjson_path)
                {
                    let _ = writeln!(f, "{line}");
                }
            }
            // Overwrite live.json
            if let Ok(pretty) = serde_json::to_string_pretty(&envelope) {
                let _ = std::fs::write(&live_path, pretty);
            }
        });
    }

    if args.verbose >= 2 {
        emitter.on_event(move |event| {
            eprint!("{}", format_event_detail(event, styles));
        });
    } else if args.verbose >= 1 {
        emitter.on_event(move |event| {
            eprintln!("{}", format_event_summary(event, styles));
        });
    } else {
        emitter.on_event(move |event| {
            match event {
                crate::event::PipelineEvent::StageCompleted { name, duration_ms, status, usage, .. } => {
                    let mut line = format!(
                        "{dim}Stage \"{name}\" completed ({status}) in {duration}",
                        duration = format_duration_human(*duration_ms),
                        dim = styles.dim,
                    );
                    if let Some(u) = usage {
                        let total = u.input_tokens + u.output_tokens;
                        let tokens_str = format_tokens_human(total);
                        if let Some(cost) = compute_stage_cost(u) {
                            line.push_str(&format!(" \u{2014} {tokens_str} tokens ({})", format_cost(cost)));
                        } else {
                            line.push_str(&format!(" \u{2014} {tokens_str} tokens"));
                        }
                    }
                    eprintln!("{line}{reset}", reset = styles.reset);
                }
                crate::event::PipelineEvent::StageFailed { name, .. } => {
                    eprintln!(
                        "{dim}Stage \"{name}\" failed{reset}",
                        dim = styles.dim, reset = styles.reset,
                    );
                }
                _ => {}
            }
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

    let run_start = Instant::now();
    let outcome = if let Some(ref checkpoint_path) = args.resume {
        let checkpoint = Checkpoint::load(checkpoint_path)?;
        engine
            .run_from_checkpoint(&graph, &config, &checkpoint)
            .await?
    } else {
        engine.run(&graph, &config).await?
    };
    let run_duration_ms = run_start.elapsed().as_millis() as u64;

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
    eprintln!("Duration: {}", format_duration_human(run_duration_ms));

    let acc = accumulator.lock().unwrap();
    let total_tokens = acc.total_input_tokens + acc.total_output_tokens;
    if total_tokens > 0 {
        if acc.has_pricing {
            eprintln!("Cost: {} ({} tokens)", format_cost(acc.total_cost), format_tokens_human(total_tokens));
        } else {
            eprintln!("Tokens: {}", format_tokens_human(total_tokens));
        }
        if acc.total_cache_read_tokens > 0 {
            eprintln!(
                "{dim}Cache: {} read, {} write{reset}",
                format_tokens_human(acc.total_cache_read_tokens),
                format_tokens_human(acc.total_cache_write_tokens),
                dim = styles.dim, reset = styles.reset,
            );
        }
        if acc.total_reasoning_tokens > 0 {
            eprintln!(
                "{dim}Reasoning: {} tokens{reset}",
                format_tokens_human(acc.total_reasoning_tokens),
                dim = styles.dim, reset = styles.reset,
            );
        }
    }
    drop(acc);

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
