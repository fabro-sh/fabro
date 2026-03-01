use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::bail;
use arc_agent::{
    DockerConfig, DockerExecutionEnvironment, ExecutionEnvironment, LocalExecutionEnvironment,
};
use arc_util::terminal::Styles;
use chrono::{Local, Utc};

use crate::checkpoint::Checkpoint;
use crate::engine::{GitCheckpointMode, PipelineEngine, RunConfig};
use crate::event::EventEmitter;
use crate::handler::default_registry;
use crate::interviewer::auto_approve::AutoApproveInterviewer;
use crate::interviewer::console::ConsoleInterviewer;
use crate::interviewer::Interviewer;
use crate::outcome::StageStatus;
use crate::pipeline::PipelineBuilder;
use crate::validation::Severity;

use arc_llm::provider::Provider;

use super::backend::AgentBackend;
use super::cli_backend::{BackendRouter, CliBackend};
use super::task_config;
use super::{
    compute_stage_cost, format_cost, format_duration_human, format_event_detail,
    format_event_summary, format_tokens_human, print_diagnostics, read_dot_file, ExecutionEnvKind,
    RunArgs,
};

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
    // Handle --run-branch resume: read everything from git metadata
    if let Some(branch) = args.run_branch.clone() {
        return run_from_branch(args, &branch, styles).await;
    }

    let pipeline_path = args
        .pipeline
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--pipeline is required unless --run-branch is provided"))?;

    // 0. Load task config if TOML, resolve DOT path, run setup
    let (dot_path, task_cfg) = if pipeline_path.extension().is_some_and(|ext| ext == "toml") {
        let cfg = task_config::load_task_config(pipeline_path)?;
        let dot = task_config::resolve_graph_path(pipeline_path, &cfg.graph);
        (dot, Some(cfg))
    } else {
        (pipeline_path.clone(), None)
    };

    if let Some(ref cfg) = task_cfg {
        if let Some(ref dir) = cfg.directory {
            std::env::set_current_dir(dir)
                .map_err(|e| anyhow::anyhow!("Failed to set working directory to {dir}: {e}"))?;
        }
    }

    // Collect setup commands — they'll be run inside the execution environment
    let setup_commands: Vec<String> = task_cfg
        .as_ref()
        .and_then(|c| c.setup.as_ref())
        .map(|s| s.commands.clone())
        .unwrap_or_default();

    // 1. Parse and validate pipeline
    let source = read_dot_file(&dot_path)?;
    let source = match task_cfg.as_ref().and_then(|c| c.vars.as_ref()) {
        Some(vars) => task_config::expand_vars(&source, vars)?,
        None => source,
    };
    let (graph, diagnostics) = PipelineBuilder::new().prepare(&source)?;

    eprintln!(
        "{bold}Parsed pipeline:{reset} {} ({dim}{} nodes, {} edges{reset})",
        graph.name,
        graph.nodes.len(),
        graph.edges.len(),
        bold = styles.bold,
        dim = styles.dim,
        reset = styles.reset,
    );

    let goal = graph.goal();
    if !goal.is_empty() {
        eprintln!(
            "{bold}Goal:{reset} {goal}",
            bold = styles.bold,
            reset = styles.reset
        );
    }

    print_diagnostics(&diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    // 2. Pre-flight: check git cleanliness before creating any files
    //    (must happen before logs dir is created, which may be inside the repo)
    let execution_env_kind_preview = {
        let toml_exec = task_cfg
            .as_ref()
            .and_then(|c| c.execution.as_ref())
            .and_then(|e| e.environment.as_deref())
            .map(|s| s.parse::<ExecutionEnvKind>())
            .transpose()
            .ok()
            .flatten();
        args.execution_env.or(toml_exec).unwrap_or_default()
    };
    let original_cwd = std::env::current_dir()?;
    let git_clean = match execution_env_kind_preview {
        ExecutionEnvKind::Local | ExecutionEnvKind::Docker => {
            crate::git::ensure_clean(&original_cwd).is_ok()
        }
        ExecutionEnvKind::Daytona => false,
    };

    // 3. Create logs directory
    let logs_dir = args.logs_dir.unwrap_or_else(|| {
        let base = dirs::home_dir()
            .expect("could not determine home directory")
            .join(".arc")
            .join("logs");
        base.join(format!("arc-run-{}", Local::now().format("%Y%m%d-%H%M%S")))
    });
    tokio::fs::create_dir_all(&logs_dir).await?;
    tokio::fs::write(logs_dir.join("graph.dot"), &source).await?;
    tokio::fs::write(logs_dir.join("run.pid"), std::process::id().to_string()).await?;
    if pipeline_path.extension().is_some_and(|ext| ext == "toml") {
        if let Ok(toml_contents) = tokio::fs::read(pipeline_path).await {
            tokio::fs::write(logs_dir.join("task.toml"), toml_contents).await?;
        }
    }

    if args.verbose >= 1 {
        eprintln!(
            "{dim}Logs: {}{reset}",
            logs_dir.display(),
            dim = styles.dim,
            reset = styles.reset,
        );
    }

    // 3. Build event emitter
    let mut emitter = EventEmitter::new();

    // Track the last git commit SHA from GitCheckpoint events
    let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let sha_clone = Arc::clone(&last_git_sha);
        emitter.on_event(move |event| {
            if let crate::event::PipelineEvent::GitCheckpoint { git_commit_sha, .. } = event {
                *sha_clone.lock().unwrap() = Some(git_commit_sha.clone());
            }
        });
    }

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
            if let crate::event::PipelineEvent::PipelineStarted { run_id, .. } = event {
                *run_id_clone.lock().unwrap() = run_id.clone();
            }
            let envelope = serde_json::json!({
                "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "run_id": *run_id_clone.lock().unwrap(),
                "event": event,
            });
            // Append to progress.ndjson
            if let Ok(line) = serde_json::to_string(&envelope) {
                let line = arc_util::redact::redact_jsonl_line(&line);
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
                let pretty = arc_util::redact::redact_jsonl_line(&pretty);
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
        emitter.on_event(move |event| match event {
            crate::event::PipelineEvent::StageCompleted {
                name,
                duration_ms,
                status,
                usage,
                ..
            } => {
                let mut line = format!(
                    "{dim}Stage \"{name}\" completed ({status}) in {duration}",
                    duration = format_duration_human(*duration_ms),
                    dim = styles.dim,
                );
                if let Some(u) = usage {
                    let total = u.input_tokens + u.output_tokens;
                    let tokens_str = format_tokens_human(total);
                    if let Some(cost) = compute_stage_cost(u) {
                        line.push_str(&format!(
                            " \u{2014} {tokens_str} tokens ({})",
                            format_cost(cost)
                        ));
                    } else {
                        line.push_str(&format!(" \u{2014} {tokens_str} tokens"));
                    }
                }
                eprintln!("{line}{reset}", reset = styles.reset);
            }
            crate::event::PipelineEvent::StageFailed { name, .. } => {
                eprintln!(
                    "{dim}Stage \"{name}\" failed{reset}",
                    dim = styles.dim,
                    reset = styles.reset,
                );
            }
            _ => {}
        });
    }

    // 4. Build interviewer
    let interviewer: Arc<dyn Interviewer> = if args.auto_approve {
        Arc::new(AutoApproveInterviewer)
    } else {
        Arc::new(ConsoleInterviewer::new(styles))
    };

    // 5. Resolve execution environment: CLI flag > TOML > default
    let toml_execution_env = task_cfg
        .as_ref()
        .and_then(|c| c.execution.as_ref())
        .and_then(|e| e.environment.as_deref())
        .map(|s| s.parse::<ExecutionEnvKind>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("Invalid execution environment in TOML: {e}"))?;
    let execution_env_kind = args
        .execution_env
        .or(toml_execution_env)
        .unwrap_or_default();

    // Set up git worktree for local execution (must happen before cwd is captured)
    let (worktree_run_id, worktree_work_dir, worktree_path, worktree_branch, worktree_base_sha) =
        if git_clean {
            match setup_worktree(&original_cwd, &logs_dir) {
                Ok((rid, wd, wt, branch, base)) => {
                    (Some(rid), Some(wd), Some(wt), Some(branch), Some(base))
                }
                Err(e) => {
                    eprintln!(
                    "{yellow}Warning:{reset} Git worktree setup failed ({e}), running without worktree.",
                    yellow = styles.yellow, reset = styles.reset,
                );
                    (None, None, None, None, None)
                }
            }
        } else {
            (None, None, None, None, None)
        };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let daytona_config = task_cfg
        .as_ref()
        .and_then(|c| c.execution.as_ref())
        .and_then(|e| e.daytona.clone());

    // Wrap emitter in Arc now so we can share it with exec env callbacks
    let emitter = Arc::new(emitter);

    let execution_env: Arc<dyn ExecutionEnvironment> = match execution_env_kind {
        ExecutionEnvKind::Docker => {
            let config = DockerConfig {
                host_working_directory: cwd.to_string_lossy().to_string(),
                ..DockerConfig::default()
            };
            let mut env = DockerExecutionEnvironment::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker environment: {e}"))?;
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::PipelineEvent::ExecutionEnv { event });
            }));
            Arc::new(env)
        }
        ExecutionEnvKind::Daytona => {
            let daytona_client = daytona_sdk::Client::new()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create Daytona client: {e}"))?;
            let config = daytona_config.clone().unwrap_or_default();
            let mut env =
                crate::daytona_env::DaytonaExecutionEnvironment::new(daytona_client, config);
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::PipelineEvent::ExecutionEnv { event });
            }));
            Arc::new(env)
        }
        ExecutionEnvKind::Local => {
            let mut env = LocalExecutionEnvironment::new(cwd);
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::PipelineEvent::ExecutionEnv { event });
            }));
            Arc::new(env)
        }
    };

    // Initialize execution environment (creates sandbox/container once for the whole pipeline)
    execution_env
        .initialize()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize execution environment: {e}"))?;

    // Ensure cleanup runs even on error/panic
    let exec_env_for_cleanup = Arc::clone(&execution_env);
    let _cleanup_guard = scopeguard::guard((), move |()| {
        // Best-effort cleanup — fire and forget in a blocking context
        let rt = tokio::runtime::Handle::try_current();
        if let Ok(handle) = rt {
            handle.spawn(async move {
                if let Err(e) = exec_env_for_cleanup.cleanup().await {
                    eprintln!("Warning: execution environment cleanup failed: {e}");
                }
            });
        }
    });

    // Set up git inside Daytona sandbox (if applicable)
    let (daytona_run_id, daytona_base_sha, daytona_branch) = if execution_env_kind
        == ExecutionEnvKind::Daytona
    {
        match setup_daytona_git(&*execution_env).await {
            Ok((rid, base, branch)) => (Some(rid), Some(base), Some(branch)),
            Err(e) => {
                eprintln!(
                    "{yellow}Warning:{reset} Daytona git setup failed ({e}), running without git checkpoints.",
                    yellow = styles.yellow, reset = styles.reset,
                );
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };

    // Run setup commands inside the execution environment (once, not per-stage)
    if !setup_commands.is_empty() {
        emitter.emit(&crate::event::PipelineEvent::SetupStarted {
            command_count: setup_commands.len(),
        });
        let setup_start = Instant::now();
        for (index, cmd) in setup_commands.iter().enumerate() {
            emitter.emit(&crate::event::PipelineEvent::SetupCommandStarted {
                command: cmd.clone(),
                index,
            });
            let cmd_start = Instant::now();
            let result = execution_env
                .exec_command(cmd, 300_000, None, None, None)
                .await
                .map_err(|e| anyhow::anyhow!("Setup command failed: {e}"))?;
            let cmd_duration = u64::try_from(cmd_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            if result.exit_code != 0 {
                emitter.emit(&crate::event::PipelineEvent::SetupFailed {
                    command: cmd.clone(),
                    index,
                    exit_code: result.exit_code,
                    stderr: result.stderr.clone(),
                });
                anyhow::bail!(
                    "Setup command failed (exit code {}): {cmd}\n{}",
                    result.exit_code,
                    result.stderr,
                );
            }
            emitter.emit(&crate::event::PipelineEvent::SetupCommandCompleted {
                command: cmd.clone(),
                index,
                exit_code: result.exit_code,
                duration_ms: cmd_duration,
            });
        }
        let setup_duration = u64::try_from(setup_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        emitter.emit(&crate::event::PipelineEvent::SetupCompleted {
            duration_ms: setup_duration,
        });
    }

    // 6. Resolve backend, model, and provider
    let (dry_run_mode, llm_client) = if args.dry_run {
        (true, None)
    } else {
        match arc_llm::client::Client::from_env().await {
            Ok(c) if c.provider_names().is_empty() => {
                eprintln!(
                    "{yellow}Warning:{reset} No LLM providers configured. Running in dry-run mode.",
                    yellow = styles.yellow,
                    reset = styles.reset,
                );
                (true, None)
            }
            Ok(c) => (false, Some(c)),
            Err(e) => {
                eprintln!(
                    "{yellow}Warning:{reset} Failed to initialize LLM client: {e}. Running in dry-run mode.",
                    yellow = styles.yellow, reset = styles.reset,
                );
                (true, None)
            }
        }
    };

    let toml_model = task_cfg
        .as_ref()
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.model.clone());
    let toml_provider = task_cfg
        .as_ref()
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.provider.clone());

    // Precedence: CLI flag > TOML > DOT graph attrs > defaults
    let provider = args.provider.or(toml_provider).or_else(|| {
        graph
            .attrs
            .get("default_provider")
            .and_then(|v| v.as_str())
            .map(String::from)
    });

    let model = args
        .model
        .or(toml_model)
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
            Some("kimi") => "kimi-k2.5".to_string(),
            Some("zai") => "glm-4.7".to_string(),
            Some("minimax") => "minimax-m2.5".to_string(),
            _ => "claude-opus-4-6".to_string(),
        });

    // Resolve model alias through catalog
    let (model, provider) = match arc_llm::catalog::get_model_info(&model) {
        Some(info) => (info.id, provider.or(Some(info.provider))),
        None => (model, provider),
    };

    // Parse provider string to enum (defaults to Anthropic)
    let provider_enum: Provider = provider
        .as_deref()
        .map(|s| s.parse::<Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or(Provider::Anthropic);

    // 7. Build engine
    let registry = default_registry(interviewer.clone(), || {
        if dry_run_mode {
            None
        } else {
            let api = AgentBackend::new(model.clone(), provider_enum, args.verbose, styles);
            let cli = CliBackend::new(model.clone(), provider_enum);
            Some(Box::new(BackendRouter::new(Box::new(api), cli)))
        }
    });
    let engine = PipelineEngine::with_interviewer(
        registry,
        Arc::clone(&emitter),
        interviewer,
        Arc::clone(&execution_env),
    );

    // 7. Execute
    let run_id = worktree_run_id
        .or(daytona_run_id)
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    // Set up metadata branch for git checkpointing (host or remote)
    let meta_branch = if worktree_work_dir.is_some() || daytona_base_sha.is_some() {
        Some(crate::git::MetadataStore::branch_name(&run_id))
    } else {
        None
    };
    let config = RunConfig {
        logs_root: logs_dir.clone(),
        cancel_token: None,
        dry_run: dry_run_mode,
        run_id,
        git_checkpoint: match execution_env_kind {
            ExecutionEnvKind::Local | ExecutionEnvKind::Docker => {
                worktree_work_dir.map(GitCheckpointMode::Host)
            }
            ExecutionEnvKind::Daytona => daytona_base_sha
                .as_ref()
                .map(|_| GitCheckpointMode::Remote(original_cwd.clone())),
        },
        base_sha: worktree_base_sha.or(daytona_base_sha),
        run_branch: worktree_branch.or(daytona_branch),
        meta_branch,
    };

    let run_start = Instant::now();
    let engine_result = if let Some(ref checkpoint_path) = args.resume {
        let checkpoint = Checkpoint::load(checkpoint_path)?;
        engine
            .run_from_checkpoint(&graph, &config, &checkpoint)
            .await
    } else {
        engine.run(&graph, &config).await
    };
    let run_duration_ms = run_start.elapsed().as_millis() as u64;

    // Restore cwd and clean up worktree (best-effort)
    let _ = std::env::set_current_dir(&original_cwd);
    if let Some(ref wt) = worktree_path {
        let _ = crate::git::remove_worktree(&original_cwd, wt);
    }

    {
        let (status, failure_reason) = match &engine_result {
            Ok(o) => (o.status.to_string(), o.failure_reason.clone()),
            Err(e) => ("fail".to_string(), Some(e.to_string())),
        };
        let mut final_json = serde_json::json!({
            "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "status": status,
            "duration_ms": run_duration_ms,
            "failure_reason": failure_reason,
        });
        if let Some(sha) = last_git_sha.lock().unwrap().clone() {
            final_json["final_git_commit_sha"] = serde_json::Value::String(sha);
        }
        if let Ok(json) = serde_json::to_string_pretty(&final_json) {
            let _ = tokio::fs::write(logs_dir.join("final.json"), json).await;
        }
    }

    // Auto-derive retro (always, cheap) and optionally run retro agent
    {
        let (failed, failure_reason) = match &engine_result {
            Ok(o) => (o.status == StageStatus::Fail, o.failure_reason.clone()),
            Err(e) => (true, Some(e.to_string())),
        };
        generate_retro(
            &config.run_id,
            &graph.name,
            graph.goal(),
            &logs_dir,
            failed,
            failure_reason.as_deref(),
            run_duration_ms,
            dry_run_mode,
            llm_client.as_ref(),
            &execution_env,
            provider_enum,
            &model,
            styles,
        )
        .await;
    }

    let outcome = engine_result?;

    // 8. Print result
    eprintln!(
        "\n{bold}=== Pipeline Result ==={reset}",
        bold = styles.bold,
        reset = styles.reset,
    );

    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => styles.green,
        _ => styles.red,
    };
    eprintln!(
        "Status: {status_color}{status_str}{reset}",
        reset = styles.reset
    );
    eprintln!("Duration: {}", format_duration_human(run_duration_ms));

    let acc = accumulator.lock().unwrap();
    let total_tokens = acc.total_input_tokens + acc.total_output_tokens;
    if total_tokens > 0 {
        if acc.has_pricing {
            eprintln!(
                "Cost: {} ({} tokens)",
                format_cost(acc.total_cost),
                format_tokens_human(total_tokens)
            );
        } else {
            eprintln!("Tokens: {}", format_tokens_human(total_tokens));
        }
        if acc.total_cache_read_tokens > 0 {
            eprintln!(
                "{dim}Cache: {} read, {} write{reset}",
                format_tokens_human(acc.total_cache_read_tokens),
                format_tokens_human(acc.total_cache_write_tokens),
                dim = styles.dim,
                reset = styles.reset,
            );
        }
        if acc.total_reasoning_tokens > 0 {
            eprintln!(
                "{dim}Reasoning: {} tokens{reset}",
                format_tokens_human(acc.total_reasoning_tokens),
                dim = styles.dim,
                reset = styles.reset,
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
            red = styles.red,
            reset = styles.reset,
        );
    }
    eprintln!(
        "{dim}Logs: {}{reset}",
        logs_dir.display(),
        dim = styles.dim,
        reset = styles.reset,
    );

    // 9. Exit code
    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => {
            std::process::exit(1);
        }
    }
}

/// Set up a git worktree for an isolated pipeline run.
/// Caller must have already verified the repo is clean via `git::ensure_clean`.
/// Returns (run_id, work_dir, worktree_path, branch_name, base_sha) on success.
fn setup_worktree(
    original_cwd: &std::path::Path,
    logs_dir: &std::path::Path,
) -> anyhow::Result<(String, PathBuf, PathBuf, String, String)> {
    let base_sha = crate::git::head_sha(original_cwd).map_err(|e| anyhow::anyhow!("{e}"))?;
    let run_id = ulid::Ulid::new().to_string();
    let branch_name = format!("arc/run/{run_id}");
    crate::git::create_branch(original_cwd, &branch_name).map_err(|e| anyhow::anyhow!("{e}"))?;

    let worktree_path = logs_dir.join("worktree");
    crate::git::replace_worktree(original_cwd, &worktree_path, &branch_name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    std::env::set_current_dir(&worktree_path)?;

    Ok((
        run_id,
        worktree_path.clone(),
        worktree_path,
        branch_name,
        base_sha,
    ))
}

/// Set up git inside a Daytona sandbox for checkpoint commits.
/// Returns (run_id, base_sha, branch_name) on success.
async fn setup_daytona_git(
    exec_env: &dyn arc_agent::ExecutionEnvironment,
) -> anyhow::Result<(String, String, String)> {
    // Get current HEAD as base SHA
    let sha_result = exec_env
        .exec_command("git rev-parse HEAD", 10_000, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("git rev-parse HEAD failed: {e}"))?;
    if sha_result.exit_code != 0 {
        anyhow::bail!(
            "git rev-parse HEAD failed (exit {}): {}",
            sha_result.exit_code,
            sha_result.stderr
        );
    }
    let base_sha = sha_result.stdout.trim().to_string();

    let run_id = ulid::Ulid::new().to_string();
    let branch_name = format!("arc/run/{run_id}");

    // Create and checkout a run branch
    let checkout_cmd = format!("git checkout -b {branch_name}");
    let checkout_result = exec_env
        .exec_command(&checkout_cmd, 10_000, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("git checkout failed: {e}"))?;
    if checkout_result.exit_code != 0 {
        anyhow::bail!(
            "git checkout -b failed (exit {}): {}",
            checkout_result.exit_code,
            checkout_result.stderr
        );
    }

    Ok((run_id, base_sha, branch_name))
}

/// Resume a pipeline run from a git run branch.
///
/// Reads the checkpoint, manifest, and graph DOT from the metadata branch
/// (`refs/arc/{run_id}`), re-attaches a worktree to the existing run branch,
/// and resumes execution via `run_from_checkpoint()`.
async fn run_from_branch(
    args: RunArgs,
    run_branch: &str,
    styles: &'static Styles,
) -> anyhow::Result<()> {
    // Extract run_id from branch name: "arc/run/{run_id}" -> "{run_id}"
    let run_id = run_branch
        .strip_prefix("arc/run/")
        .ok_or_else(|| {
            anyhow::anyhow!(
                "invalid run branch format: expected 'arc/run/<run_id>', got '{run_branch}'"
            )
        })?
        .to_string();

    let original_cwd = std::env::current_dir()?;

    // Read checkpoint from metadata branch
    let checkpoint = crate::git::MetadataStore::read_checkpoint(&original_cwd, &run_id)?
        .ok_or_else(|| {
            anyhow::anyhow!("no checkpoint found on metadata branch for run {run_id}")
        })?;

    // Read graph DOT from metadata branch
    let source = crate::git::MetadataStore::read_graph_dot(&original_cwd, &run_id)?
        .ok_or_else(|| anyhow::anyhow!("no graph.dot found on metadata branch for run {run_id}"))?;

    // If --pipeline was also provided, use it instead (allows overriding)
    let source = if let Some(ref pipeline_path) = args.pipeline {
        super::read_dot_file(pipeline_path)?
    } else {
        source
    };

    let (graph, diagnostics) = crate::pipeline::PipelineBuilder::new().prepare(&source)?;

    eprintln!(
        "{bold}Resuming pipeline:{reset} {} from branch {dim}{run_branch}{reset}",
        graph.name,
        bold = styles.bold,
        dim = styles.dim,
        reset = styles.reset,
    );

    super::print_diagnostics(&diagnostics, styles);
    if diagnostics
        .iter()
        .any(|d| d.severity == crate::validation::Severity::Error)
    {
        anyhow::bail!("Validation failed");
    }

    // Set up logs directory
    let logs_dir = args.logs_dir.unwrap_or_else(|| {
        let base = dirs::home_dir()
            .expect("could not determine home directory")
            .join(".arc")
            .join("logs");
        base.join(format!(
            "arc-resume-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    tokio::fs::create_dir_all(&logs_dir).await?;
    tokio::fs::write(logs_dir.join("graph.dot"), &source).await?;

    // Re-attach worktree to the existing run branch
    let worktree_path = logs_dir.join("worktree");
    crate::git::replace_worktree(&original_cwd, &worktree_path, run_branch)
        .map_err(|e| anyhow::anyhow!("failed to attach worktree to {run_branch}: {e}"))?;
    std::env::set_current_dir(&worktree_path)?;

    let base_sha = crate::git::MetadataStore::read_manifest(&original_cwd, &run_id)?
        .and_then(|m| m.get("base_sha").and_then(|v| v.as_str()).map(String::from));

    // Build minimal execution environment (local only for now)
    let emitter = Arc::new(EventEmitter::new());
    let execution_env: Arc<dyn arc_agent::ExecutionEnvironment> = {
        let mut env = arc_agent::LocalExecutionEnvironment::new(worktree_path.clone());
        let emitter_cb = Arc::clone(&emitter);
        env.set_event_callback(Arc::new(move |event| {
            emitter_cb.emit(&crate::event::PipelineEvent::ExecutionEnv { event });
        }));
        Arc::new(env)
    };

    // Build interviewer
    let interviewer: Arc<dyn crate::interviewer::Interviewer> = if args.auto_approve {
        Arc::new(crate::interviewer::auto_approve::AutoApproveInterviewer)
    } else {
        Arc::new(crate::interviewer::console::ConsoleInterviewer::new(styles))
    };

    // Build engine with a backend
    let dry_run_mode = args.dry_run
        || arc_llm::client::Client::from_env()
            .await
            .map(|c| c.provider_names().is_empty())
            .unwrap_or(true);

    let model = args.model.unwrap_or_else(|| "claude-opus-4-6".to_string());
    let provider_enum = args
        .provider
        .as_deref()
        .map(|s| s.parse::<arc_llm::provider::Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or(arc_llm::provider::Provider::Anthropic);

    let registry = crate::handler::default_registry(interviewer.clone(), || {
        if dry_run_mode {
            None
        } else {
            let api = AgentBackend::new(model.clone(), provider_enum, args.verbose, styles);
            let cli = CliBackend::new(model.clone(), provider_enum);
            Some(Box::new(BackendRouter::new(Box::new(api), cli)))
        }
    });
    let engine = crate::engine::PipelineEngine::with_interviewer(
        registry,
        Arc::clone(&emitter),
        interviewer,
        Arc::clone(&execution_env),
    );

    let meta_branch = Some(crate::git::MetadataStore::branch_name(&run_id));
    let config = RunConfig {
        logs_root: logs_dir.clone(),
        cancel_token: None,
        dry_run: dry_run_mode,
        run_id,
        git_checkpoint: Some(GitCheckpointMode::Host(worktree_path.clone())),
        base_sha,
        run_branch: Some(run_branch.to_string()),
        meta_branch,
    };

    let run_start = Instant::now();
    let engine_result = engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await;
    let run_duration_ms = run_start.elapsed().as_millis() as u64;

    // Clean up
    let _ = std::env::set_current_dir(&original_cwd);
    let _ = crate::git::remove_worktree(&original_cwd, &worktree_path);

    // Auto-derive retro
    {
        let (failed, failure_reason) = match &engine_result {
            Ok(o) => (o.status == StageStatus::Fail, o.failure_reason.clone()),
            Err(e) => (true, Some(e.to_string())),
        };

        let llm_client = if dry_run_mode {
            None
        } else {
            arc_llm::client::Client::from_env().await.ok()
        };

        generate_retro(
            &config.run_id,
            &graph.name,
            graph.goal(),
            &logs_dir,
            failed,
            failure_reason.as_deref(),
            run_duration_ms,
            dry_run_mode,
            llm_client.as_ref(),
            &execution_env,
            provider_enum,
            &model,
            styles,
        )
        .await;
    }

    let outcome = engine_result?;

    eprintln!(
        "\n{bold}=== Pipeline Result ==={reset}",
        bold = styles.bold,
        reset = styles.reset,
    );
    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => styles.green,
        _ => styles.red,
    };
    eprintln!(
        "Status: {status_color}{status_str}{reset}",
        reset = styles.reset
    );
    eprintln!(
        "Duration: {}",
        super::format_duration_human(run_duration_ms)
    );
    eprintln!(
        "{dim}Logs: {}{reset}",
        logs_dir.display(),
        dim = styles.dim,
        reset = styles.reset,
    );

    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => std::process::exit(1),
    }
}

/// Generate a retro report for a completed pipeline run.
///
/// Derives a basic retro from the checkpoint, then optionally runs the retro agent
/// for a richer narrative. Errors are logged as warnings rather than propagated.
#[allow(clippy::too_many_arguments)]
async fn generate_retro(
    run_id: &str,
    pipeline_name: &str,
    goal: &str,
    logs_dir: &std::path::Path,
    failed: bool,
    failure_reason: Option<&str>,
    run_duration_ms: u64,
    dry_run_mode: bool,
    llm_client: Option<&arc_llm::client::Client>,
    execution_env: &Arc<dyn arc_agent::ExecutionEnvironment>,
    provider_enum: Provider,
    model: &str,
    styles: &'static Styles,
) {
    let cp = match Checkpoint::load(&logs_dir.join("checkpoint.json")) {
        Ok(cp) => cp,
        Err(e) => {
            eprintln!(
                "{yellow}Warning:{reset} Could not load checkpoint, skipping retro: {e}",
                yellow = styles.yellow,
                reset = styles.reset,
            );
            return;
        }
    };

    let stage_durations = crate::retro::extract_stage_durations(logs_dir);
    let mut retro = crate::retro::derive_retro(
        run_id,
        pipeline_name,
        goal,
        &cp,
        failed,
        failure_reason,
        run_duration_ms,
        &stage_durations,
    );

    match retro.save(logs_dir) {
        Ok(()) => {}
        Err(e) => {
            eprintln!(
                "{yellow}Warning:{reset} Failed to save initial retro: {e}",
                yellow = styles.yellow,
                reset = styles.reset,
            );
        }
    }

    // Run retro agent session
    let narrative_result = if dry_run_mode {
        Ok(crate::retro_agent::dry_run_narrative())
    } else if let Some(client) = llm_client {
        crate::retro_agent::run_retro_agent(execution_env, logs_dir, client, provider_enum, model)
            .await
    } else {
        Err(anyhow::anyhow!("No LLM client available"))
    };

    match narrative_result {
        Ok(narrative) => {
            retro.apply_narrative(narrative);
            match retro.save(logs_dir) {
                Ok(()) => {
                    eprintln!(
                        "{dim}Retro saved to {}/retro.json{reset}",
                        logs_dir.display(),
                        dim = styles.dim,
                        reset = styles.reset,
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{yellow}Warning:{reset} Failed to save retro with narrative: {e}",
                        yellow = styles.yellow,
                        reset = styles.reset,
                    );
                }
            }
        }
        Err(e) => {
            eprintln!(
                "{dim}Retro agent skipped: {e}{reset}",
                dim = styles.dim,
                reset = styles.reset,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn redact_removes_aws_key_from_compact_json() {
        let envelope = serde_json::json!({
            "timestamp": "2025-01-01T00:00:00.000Z",
            "run_id": "abc-123",
            "event": {
                "type": "agent",
                "content": "My key is AKIAYRWQG5EJLPZLBYNP and secret is wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
            }
        });
        let compact = serde_json::to_string(&envelope).unwrap();
        let redacted = arc_util::redact::redact_jsonl_line(&compact);

        assert!(!redacted.contains("AKIAYRWQG5EJLPZLBYNP"));
        assert!(redacted.contains("REDACTED"));

        let parsed: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(parsed["run_id"], "abc-123");
        assert_eq!(parsed["timestamp"], "2025-01-01T00:00:00.000Z");
    }

    #[test]
    fn redact_removes_aws_key_from_pretty_json() {
        let envelope = serde_json::json!({
            "timestamp": "2025-01-01T00:00:00.000Z",
            "run_id": "def-456",
            "event": {
                "type": "agent",
                "content": "Credentials: AKIAYRWQG5EJLPZLBYNP"
            }
        });
        let pretty = serde_json::to_string_pretty(&envelope).unwrap();
        let redacted = arc_util::redact::redact_jsonl_line(&pretty);

        assert!(!redacted.contains("AKIAYRWQG5EJLPZLBYNP"));
        assert!(redacted.contains("REDACTED"));

        let parsed: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(parsed["run_id"], "def-456");
    }
}
