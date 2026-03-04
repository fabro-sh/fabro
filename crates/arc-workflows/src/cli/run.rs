use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::bail;
use arc_agent::{DockerSandbox, DockerSandboxConfig, LocalSandbox, Sandbox};
use arc_util::terminal::Styles;
use chrono::{Local, Utc};

use crate::checkpoint::Checkpoint;
use crate::engine::{GitCheckpointMode, RunConfig, WorkflowRunEngine};
use crate::event::EventEmitter;
use crate::handler::default_registry;
use crate::interviewer::auto_approve::AutoApproveInterviewer;
use crate::interviewer::console::ConsoleInterviewer;
use crate::interviewer::Interviewer;
use crate::outcome::StageStatus;
use crate::validation::Severity;
use crate::workflow::WorkflowBuilder;

use arc_llm::provider::Provider;

use super::backend::AgentApiBackend;
use super::cli_backend::{AgentCliBackend, BackendRouter};
use super::progress;
use super::run_config;
use super::run_config::{RunDefaults, WorkflowRunConfig};
use indicatif::HumanDuration;
use std::time::Duration;

use super::{
    compute_stage_cost, format_cost, format_tokens_human, print_diagnostics, read_dot_file,
    RunArgs, SandboxProvider,
};

/// Return the default model string for a given provider.
fn default_model_for_provider(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "claude-opus-4-6",
        Provider::OpenAi => "gpt-5.2",
        Provider::Gemini => "gemini-3.1-pro-preview",
        Provider::Kimi => "kimi-k2.5",
        Provider::Zai => "glm-4.7",
        Provider::Minimax => "minimax-m2.5",
        Provider::Inception => "mercury",
    }
}

/// Resolve model and provider through the full precedence chain:
/// CLI flag > TOML config > run defaults > DOT graph attrs > provider-specific defaults.
/// Then resolve through the catalog for alias expansion.
fn resolve_model_provider(
    cli_model: Option<&str>,
    cli_provider: Option<&str>,
    run_cfg: Option<&WorkflowRunConfig>,
    run_defaults: &RunDefaults,
    graph: &crate::graph::types::Graph,
) -> (String, Option<String>) {
    let toml_model = run_cfg
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.model.as_deref());
    let toml_provider = run_cfg
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.provider.as_deref());
    let defaults_model = run_defaults.llm.as_ref().and_then(|l| l.model.as_deref());
    let defaults_provider = run_defaults
        .llm
        .as_ref()
        .and_then(|l| l.provider.as_deref());

    // Precedence: CLI flag > TOML > run defaults > DOT graph attrs > defaults
    let provider = cli_provider
        .or(toml_provider)
        .or(defaults_provider)
        .or_else(|| graph.attrs.get("default_provider").and_then(|v| v.as_str()))
        .map(String::from);

    let model = cli_model
        .or(toml_model)
        .or(defaults_model)
        .or_else(|| graph.attrs.get("default_model").and_then(|v| v.as_str()))
        .map(String::from)
        .unwrap_or_else(|| {
            let provider_enum = provider
                .as_deref()
                .and_then(|s| s.parse::<Provider>().ok())
                .unwrap_or(Provider::Anthropic);
            default_model_for_provider(provider_enum).to_string()
        });

    // Resolve model alias through catalog
    match arc_llm::catalog::get_model_info(&model) {
        Some(info) => (info.id, provider.or(Some(info.provider))),
        None => (model, provider),
    }
}

/// Parse sandbox provider from an optional `SandboxConfig`.
fn parse_sandbox_provider(
    sandbox: Option<&run_config::SandboxConfig>,
) -> anyhow::Result<Option<SandboxProvider>> {
    sandbox
        .and_then(|s| s.provider.as_deref())
        .map(|s| s.parse::<SandboxProvider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("Invalid sandbox provider: {e}"))
}

/// Resolve sandbox provider: CLI flag > TOML config > run defaults > default.
fn resolve_sandbox_provider(
    cli: Option<SandboxProvider>,
    run_cfg: Option<&WorkflowRunConfig>,
    run_defaults: &RunDefaults,
) -> anyhow::Result<SandboxProvider> {
    let toml = parse_sandbox_provider(run_cfg.and_then(|c| c.sandbox.as_ref()))?;
    let defaults = parse_sandbox_provider(run_defaults.sandbox.as_ref())?;
    Ok(cli.or(toml).or(defaults).unwrap_or_default())
}

/// Resolve preserve-sandbox: CLI flag > TOML config > run defaults > false.
fn resolve_preserve_sandbox(
    cli: bool,
    run_cfg: Option<&WorkflowRunConfig>,
    run_defaults: &RunDefaults,
) -> bool {
    if cli {
        return true;
    }
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|s| s.preserve)
        .or_else(|| run_defaults.sandbox.as_ref().and_then(|s| s.preserve))
        .unwrap_or(false)
}

/// Resolve daytona config: TOML config > run defaults.
fn resolve_daytona_config(
    run_cfg: Option<&WorkflowRunConfig>,
    run_defaults: &RunDefaults,
) -> Option<crate::daytona_sandbox::DaytonaConfig> {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|e| e.daytona.clone())
        .or_else(|| {
            run_defaults
                .sandbox
                .as_ref()
                .and_then(|s| s.daytona.clone())
        })
}

/// Accumulates token usage and cost across all workflow stages.
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

/// Execute a full workflow run.
///
/// # Errors
///
/// Returns an error if the workflow cannot be read, parsed, validated, or executed.
pub async fn run_command(
    args: RunArgs,
    run_defaults: RunDefaults,
    styles: &'static Styles,
) -> anyhow::Result<()> {
    // Handle --run-branch resume: read everything from git metadata
    if let Some(branch) = args.run_branch.clone() {
        return run_from_branch(args, &branch, styles).await;
    }

    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required unless --run-branch is provided"))?;

    // 0. Load run config if TOML, resolve DOT path, apply defaults
    let (dot_path, run_cfg) = if workflow_path.extension().is_some_and(|ext| ext == "toml") {
        let mut cfg = run_config::load_run_config(workflow_path)?;
        cfg.apply_defaults(&run_defaults);
        let dot = run_config::resolve_graph_path(workflow_path, &cfg.graph);
        (dot, Some(cfg))
    } else {
        (workflow_path.clone(), None)
    };

    let directory = run_cfg
        .as_ref()
        .and_then(|c| c.directory.as_deref())
        .or(run_defaults.directory.as_deref());
    if let Some(dir) = directory {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("Failed to set working directory to {dir}: {e}"))?;
    }

    // Collect setup commands — they'll be run inside the sandbox
    let setup_commands: Vec<String> = run_cfg
        .as_ref()
        .and_then(|c| c.setup.as_ref())
        .or(run_defaults.setup.as_ref())
        .map(|s| s.commands.clone())
        .unwrap_or_default();

    // 1. Parse and validate workflow
    let source = read_dot_file(&dot_path)?;
    let vars = run_cfg
        .as_ref()
        .and_then(|c| c.vars.as_ref())
        .or(run_defaults.vars.as_ref());
    let source = match vars {
        Some(vars) => run_config::expand_vars(&source, vars)?,
        None => source,
    };
    let (graph, diagnostics) = WorkflowBuilder::new().prepare(&source)?;

    eprintln!(
        "{} {} {}",
        styles.bold.apply_to("Workflow:"),
        graph.name,
        styles.dim.apply_to(format!(
            "({} nodes, {} edges)",
            graph.nodes.len(),
            graph.edges.len()
        )),
    );

    let goal = graph.goal();
    if !goal.is_empty() {
        eprintln!("{} {goal}\n", styles.bold.apply_to("Goal:"));
    }

    print_diagnostics(&diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    // 2. Pre-flight: check git cleanliness before creating any files
    //    (must happen before logs dir is created, which may be inside the repo)
    let sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(args.sandbox, run_cfg.as_ref(), &run_defaults)?
    };
    let preserve_sandbox =
        resolve_preserve_sandbox(args.preserve_sandbox, run_cfg.as_ref(), &run_defaults);
    let original_cwd = std::env::current_dir()?;
    let git_clean = match sandbox_provider {
        SandboxProvider::Local | SandboxProvider::Docker => {
            crate::git::ensure_clean(&original_cwd).is_ok()
        }
        SandboxProvider::Daytona => false,
    };

    if args.preflight {
        return run_preflight(
            &graph,
            &run_cfg,
            &args,
            &run_defaults,
            git_clean,
            sandbox_provider,
            styles,
        )
        .await;
    }

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
    if workflow_path.extension().is_some_and(|ext| ext == "toml") {
        if let Ok(toml_contents) = tokio::fs::read(workflow_path).await {
            tokio::fs::write(logs_dir.join("run.toml"), toml_contents).await?;
        }
    }

    // Create progress UI (used for both normal and verbose modes)
    let is_tty = std::io::stderr().is_terminal();
    let progress_ui = Arc::new(Mutex::new(progress::ProgressUI::new(is_tty, args.verbose)));

    progress_ui
        .lock()
        .expect("progress lock poisoned")
        .show_logs_dir(&logs_dir);

    // 3. Build event emitter
    let mut emitter = EventEmitter::new();

    // Track the last git commit SHA from GitCheckpoint events
    let last_git_sha: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let sha_clone = Arc::clone(&last_git_sha);
        emitter.on_event(move |event| {
            if let crate::event::WorkflowRunEvent::GitCheckpoint { git_commit_sha, .. } = event {
                *sha_clone.lock().unwrap() = Some(git_commit_sha.clone());
            }
        });
    }

    // Cost accumulator — shared across all verbosity levels
    let accumulator = Arc::new(Mutex::new(CostAccumulator::default()));
    let acc_clone = Arc::clone(&accumulator);
    emitter.on_event(move |event| {
        if let crate::event::WorkflowRunEvent::StageCompleted { usage: Some(u), .. } = event {
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

    // JSONL progress log + live.json snapshot
    {
        let jsonl_path = logs_dir.join("progress.jsonl");
        let live_path = logs_dir.join("live.json");
        let run_id = Arc::new(Mutex::new(String::new()));
        let run_id_clone = Arc::clone(&run_id);
        emitter.on_event(move |event| {
            if let crate::event::WorkflowRunEvent::WorkflowRunStarted { run_id, .. } = event {
                *run_id_clone.lock().unwrap() = run_id.clone();
            }
            let (event_name, event_fields) = crate::event::flatten_event(event);
            let mut envelope = serde_json::Map::new();
            envelope.insert(
                "ts".to_string(),
                serde_json::Value::String(
                    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                ),
            );
            envelope.insert(
                "run_id".to_string(),
                serde_json::Value::String(run_id_clone.lock().unwrap().clone()),
            );
            envelope.insert("event".to_string(), serde_json::Value::String(event_name));
            for (k, v) in event_fields {
                if k != "ts" && k != "run_id" && k != "event" {
                    envelope.insert(k, v);
                }
            }
            let envelope = serde_json::Value::Object(envelope);
            // Append to progress.jsonl
            if let Ok(line) = serde_json::to_string(&envelope) {
                let line = arc_util::redact::redact_jsonl_line(&line);
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&jsonl_path)
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

    progress::ProgressUI::register(&progress_ui, &mut emitter);

    // 4. Build interviewer
    let interviewer: Arc<dyn Interviewer> = if args.auto_approve {
        Arc::new(AutoApproveInterviewer)
    } else {
        Arc::new(progress::ProgressAwareInterviewer::new(
            ConsoleInterviewer::new(styles),
            Arc::clone(&progress_ui),
        ))
    };

    // Set up git worktree for local execution (must happen before cwd is captured)
    let (worktree_run_id, worktree_work_dir, worktree_path, worktree_branch, worktree_base_sha) =
        if git_clean {
            match setup_worktree(&original_cwd, &logs_dir) {
                Ok((rid, wd, wt, branch, base)) => {
                    (Some(rid), Some(wd), Some(wt), Some(branch), Some(base))
                }
                Err(e) => {
                    eprintln!(
                        "{} Git worktree setup failed ({e}), running without worktree.",
                        styles.yellow.apply_to("Warning:"),
                    );
                    (None, None, None, None, None)
                }
            }
        } else {
            (None, None, None, None, None)
        };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let daytona_config = resolve_daytona_config(run_cfg.as_ref(), &run_defaults);

    // Wrap emitter in Arc now so we can share it with exec env callbacks
    let emitter = Arc::new(emitter);

    let mut daytona_sandbox_ref: Option<Arc<crate::daytona_sandbox::DaytonaSandbox>> = None;
    let sandbox: Arc<dyn Sandbox> = match sandbox_provider {
        SandboxProvider::Docker => {
            let config = DockerSandboxConfig {
                host_working_directory: cwd.to_string_lossy().to_string(),
                ..DockerSandboxConfig::default()
            };
            let mut env = DockerSandbox::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker environment: {e}"))?;
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Daytona => {
            let daytona_client = daytona_sdk::Client::new()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create Daytona client: {e}"))?;
            let config = daytona_config.clone().unwrap_or_default();
            let mut env = crate::daytona_sandbox::DaytonaSandbox::new(daytona_client, config);
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
            }));
            let daytona_arc = Arc::new(env);
            daytona_sandbox_ref = Some(Arc::clone(&daytona_arc));
            daytona_arc
        }
        SandboxProvider::Local => {
            let mut env = LocalSandbox::new(cwd);
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
    };

    // Initialize sandbox (creates sandbox/container once for the whole run)
    sandbox
        .initialize()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize sandbox: {e}"))?;

    // Safety net: if we panic or return early, best-effort cleanup via spawn.
    let sandbox_for_cleanup = Arc::clone(&sandbox);
    let cleanup_guard = scopeguard::guard((), move |()| {
        if preserve_sandbox {
            return;
        }
        let rt = tokio::runtime::Handle::try_current();
        if let Ok(handle) = rt {
            handle.spawn(async move {
                let _ = sandbox_for_cleanup.cleanup().await;
            });
        }
    });

    // Set up git inside Daytona sandbox (if applicable)
    let (daytona_run_id, daytona_base_sha, daytona_branch) =
        if sandbox_provider == SandboxProvider::Daytona {
            match setup_daytona_git(&*sandbox).await {
                Ok((rid, base, branch)) => (Some(rid), Some(base), Some(branch)),
                Err(e) => {
                    eprintln!(
                        "{} Daytona git setup failed ({e}), running without git checkpoints.",
                        styles.yellow.apply_to("Warning:"),
                    );
                    (None, None, None)
                }
            }
        } else {
            (None, None, None)
        };

    // Create SSH access if requested
    if args.ssh {
        if let Some(ref daytona) = daytona_sandbox_ref {
            match daytona.create_ssh_access().await {
                Ok(ssh_command) => {
                    emitter.emit(&crate::event::WorkflowRunEvent::SshAccessReady {
                        ssh_command,
                    });
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to create SSH access: {e}",
                        styles.yellow.apply_to("Warning:"),
                    );
                }
            }
        } else {
            eprintln!(
                "{} --ssh only works with --sandbox daytona, skipping.",
                styles.yellow.apply_to("Warning:"),
            );
        }
    }

    // Run setup commands inside the sandbox (once, not per-stage)
    if !setup_commands.is_empty() {
        emitter.emit(&crate::event::WorkflowRunEvent::SetupStarted {
            command_count: setup_commands.len(),
        });
        let setup_start = Instant::now();
        for (index, cmd) in setup_commands.iter().enumerate() {
            emitter.emit(&crate::event::WorkflowRunEvent::SetupCommandStarted {
                command: cmd.clone(),
                index,
            });
            let cmd_start = Instant::now();
            let result = sandbox
                .exec_command(cmd, 300_000, None, None, None)
                .await
                .map_err(|e| anyhow::anyhow!("Setup command failed: {e}"))?;
            let cmd_duration = u64::try_from(cmd_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            if result.exit_code != 0 {
                emitter.emit(&crate::event::WorkflowRunEvent::SetupFailed {
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
            emitter.emit(&crate::event::WorkflowRunEvent::SetupCommandCompleted {
                command: cmd.clone(),
                index,
                exit_code: result.exit_code,
                duration_ms: cmd_duration,
            });
        }
        let setup_duration = u64::try_from(setup_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        emitter.emit(&crate::event::WorkflowRunEvent::SetupCompleted {
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
                    "{} No LLM providers configured. Running in dry-run mode.",
                    styles.yellow.apply_to("Warning:"),
                );
                (true, None)
            }
            Ok(c) => (false, Some(c)),
            Err(e) => {
                eprintln!(
                    "{} Failed to initialize LLM client: {e}. Running in dry-run mode.",
                    styles.yellow.apply_to("Warning:"),
                );
                (true, None)
            }
        }
    };

    let (model, provider) = resolve_model_provider(
        args.model.as_deref(),
        args.provider.as_deref(),
        run_cfg.as_ref(),
        &run_defaults,
        &graph,
    );

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
            let api = AgentApiBackend::new(model.clone(), provider_enum);
            let cli = AgentCliBackend::new(model.clone(), provider_enum);
            Some(Box::new(BackendRouter::new(Box::new(api), cli)))
        }
    });
    let engine = WorkflowRunEngine::with_interviewer(
        registry,
        Arc::clone(&emitter),
        interviewer,
        Arc::clone(&sandbox),
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
        git_checkpoint: match sandbox_provider {
            SandboxProvider::Local | SandboxProvider::Docker => {
                worktree_work_dir.map(GitCheckpointMode::Host)
            }
            SandboxProvider::Daytona => daytona_base_sha
                .as_ref()
                .map(|_| GitCheckpointMode::Remote(original_cwd.clone())),
        },
        base_sha: worktree_base_sha.or(daytona_base_sha),
        run_branch: worktree_branch.or(daytona_branch),
        meta_branch,
        labels: args
            .label
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
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
            Ok(o) => (o.status.to_string(), o.failure_reason().map(String::from)),
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

    // Finish progress bars before printing summary
    progress_ui.lock().expect("progress lock poisoned").finish();

    // Auto-derive retro (always, cheap) and optionally run retro agent
    if !args.no_retro {
        let (failed, failure_reason) = match &engine_result {
            Ok(o) => (
                o.status == StageStatus::Fail,
                o.failure_reason().map(String::from),
            ),
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
            &sandbox,
            provider_enum,
            &model,
            styles,
        )
        .await;
    }

    let outcome = engine_result?;

    // 8. Print result
    eprintln!("\n{}", styles.bold.apply_to("=== Run Result ==="),);

    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => &styles.bold_green,
        _ => &styles.bold_red,
    };
    eprintln!("Status: {}", status_color.apply_to(&status_str),);
    eprintln!(
        "Duration: {}",
        HumanDuration(Duration::from_millis(run_duration_ms))
    );

    let acc = accumulator.lock().unwrap();
    let total_tokens = acc.total_input_tokens + acc.total_output_tokens;
    if total_tokens > 0 {
        if acc.has_pricing {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!(
                    "Cost: {} ({} tokens)",
                    format_cost(acc.total_cost),
                    format_tokens_human(total_tokens)
                ))
            );
        } else {
            eprintln!("{}", styles.dim.apply_to(format!("Tokens: {}", format_tokens_human(total_tokens))));
        }
        if acc.total_cache_read_tokens > 0 {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!(
                    "Cache: {} read, {} write",
                    format_tokens_human(acc.total_cache_read_tokens),
                    format_tokens_human(acc.total_cache_write_tokens),
                )),
            );
        }
        if acc.total_reasoning_tokens > 0 {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!(
                    "Reasoning: {} tokens",
                    format_tokens_human(acc.total_reasoning_tokens),
                )),
            );
        }
    }
    drop(acc);

    if let Some(failure) = outcome.failure_reason() {
        eprintln!("{}", styles.red.apply_to(format!("Failure: {failure}")),);
    }

    print_final_output(&logs_dir, styles);

    // 9. Cleanup sandbox (defuse the scopeguard so we await properly)
    scopeguard::ScopeGuard::into_inner(cleanup_guard);
    if preserve_sandbox {
        let info = sandbox.sandbox_info();
        if !info.is_empty() {
            eprintln!(
                "\n{} sandbox preserved: {info}",
                styles.bold.apply_to("Info:")
            );
        } else {
            eprintln!(
                "\n{} sandbox preserved",
                styles.bold.apply_to("Info:")
            );
        }
    } else if let Err(e) = sandbox.cleanup().await {
        tracing::warn!(error = %e, "Sandbox cleanup failed");
        eprintln!(
            "\n{} sandbox cleanup failed: {e}",
            styles.yellow.apply_to("Warning:")
        );
    }

    // 10. Exit code
    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => {
            std::process::exit(1);
        }
    }
}

/// Set up a git worktree for an isolated workflow run.
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
    sandbox: &dyn arc_agent::Sandbox,
) -> anyhow::Result<(String, String, String)> {
    // Get current HEAD as base SHA
    let sha_result = sandbox
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
    let checkout_result = sandbox
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

/// Resume a workflow run from a git run branch.
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
    let source = if let Some(ref workflow_path) = args.workflow {
        super::read_dot_file(workflow_path)?
    } else {
        source
    };

    let (graph, diagnostics) = crate::workflow::WorkflowBuilder::new().prepare(&source)?;

    eprintln!(
        "{} {} from branch {}",
        styles.bold.apply_to("Resuming workflow:"),
        graph.name,
        styles.dim.apply_to(run_branch),
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

    // Build minimal sandbox (local only for now)
    let emitter = Arc::new(EventEmitter::new());
    let sandbox: Arc<dyn arc_agent::Sandbox> = {
        let mut env = arc_agent::LocalSandbox::new(worktree_path.clone());
        let emitter_cb = Arc::clone(&emitter);
        env.set_event_callback(Arc::new(move |event| {
            emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
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
            let api = AgentApiBackend::new(model.clone(), provider_enum);
            let cli = AgentCliBackend::new(model.clone(), provider_enum);
            Some(Box::new(BackendRouter::new(Box::new(api), cli)))
        }
    });
    let engine = crate::engine::WorkflowRunEngine::with_interviewer(
        registry,
        Arc::clone(&emitter),
        interviewer,
        Arc::clone(&sandbox),
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
        labels: HashMap::new(),
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
    if !args.no_retro {
        let (failed, failure_reason) = match &engine_result {
            Ok(o) => (
                o.status == StageStatus::Fail,
                o.failure_reason().map(String::from),
            ),
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
            &sandbox,
            provider_enum,
            &model,
            styles,
        )
        .await;
    }

    let outcome = engine_result?;

    eprintln!("\n{}", styles.bold.apply_to("=== Run Result ==="),);
    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => &styles.bold_green,
        _ => &styles.bold_red,
    };
    eprintln!("Status: {}", status_color.apply_to(&status_str),);
    eprintln!(
        "Duration: {}",
        HumanDuration(Duration::from_millis(run_duration_ms))
    );

    print_final_output(&logs_dir, styles);

    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => std::process::exit(1),
    }
}

/// Print the final stage output from the checkpoint, if available.
fn print_final_output(logs_dir: &std::path::Path, styles: &Styles) {
    let Ok(checkpoint) = Checkpoint::load(&logs_dir.join("checkpoint.json")) else {
        return;
    };

    // Find the last stage that produced a response (walk completed_nodes in reverse,
    // looking for a "response.{node_id}" entry in context_values).
    for node_id in checkpoint.completed_nodes.iter().rev() {
        let key = format!("response.{node_id}");
        if let Some(serde_json::Value::String(response)) = checkpoint.context_values.get(&key) {
            let text = response.trim();
            if !text.is_empty() {
                eprintln!("\n{}", styles.bold.apply_to("=== Output ==="));
                eprintln!("{text}");
            }
            return;
        }
    }
}

/// Validate run configuration without executing the workflow.
///
/// Boots the sandbox (init + cleanup), checks LLM provider availability,
/// resolves the model/provider through the full precedence chain, and prints
/// a structured report.
async fn run_preflight(
    graph: &crate::graph::types::Graph,
    run_cfg: &Option<run_config::WorkflowRunConfig>,
    args: &RunArgs,
    run_defaults: &RunDefaults,
    git_clean: bool,
    sandbox_provider: SandboxProvider,
    styles: &'static Styles,
) -> anyhow::Result<()> {
    let mut errors: Vec<String> = Vec::new();

    // 1. Sandbox boot check
    let original_cwd = std::env::current_dir()?;
    let daytona_config = resolve_daytona_config(run_cfg.as_ref(), run_defaults);

    let sandbox_result: Result<Arc<dyn Sandbox>, String> = match sandbox_provider {
        SandboxProvider::Docker => {
            let config = DockerSandboxConfig {
                host_working_directory: original_cwd.to_string_lossy().to_string(),
                ..DockerSandboxConfig::default()
            };
            DockerSandbox::new(config)
                .map(|env| Arc::new(env) as Arc<dyn Sandbox>)
                .map_err(|e| format!("Docker sandbox creation failed: {e}"))
        }
        SandboxProvider::Daytona => match daytona_sdk::Client::new().await {
            Ok(daytona_client) => {
                let config = daytona_config.unwrap_or_default();
                let env = crate::daytona_sandbox::DaytonaSandbox::new(daytona_client, config);
                Ok(Arc::new(env) as Arc<dyn Sandbox>)
            }
            Err(e) => Err(format!("Daytona client creation failed: {e}")),
        },
        SandboxProvider::Local => {
            Ok(Arc::new(LocalSandbox::new(original_cwd.clone())) as Arc<dyn Sandbox>)
        }
    };

    let sandbox_ready = match sandbox_result {
        Ok(sandbox) => match sandbox.initialize().await {
            Ok(()) => {
                let _ = sandbox.cleanup().await;
                true
            }
            Err(e) => {
                errors.push(format!("Sandbox init failed: {e}"));
                let _ = sandbox.cleanup().await;
                false
            }
        },
        Err(e) => {
            errors.push(e);
            false
        }
    };

    // 2. LLM client check
    let (llm_available, llm_providers) = match arc_llm::client::Client::from_env().await {
        Ok(c) => {
            let names = c
                .provider_names()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>();
            if names.is_empty() {
                errors.push("No LLM providers configured (no API keys found)".to_string());
                (false, names)
            } else {
                (true, names)
            }
        }
        Err(e) => {
            errors.push(format!("LLM client init failed: {e}"));
            (false, Vec::new())
        }
    };

    // 3. Model/provider resolution
    let (model, provider) = resolve_model_provider(
        args.model.as_deref(),
        args.provider.as_deref(),
        run_cfg.as_ref(),
        run_defaults,
        graph,
    );

    // 4. Provider parse check
    let provider_valid = if let Some(ref p) = provider {
        match p.parse::<Provider>() {
            Ok(_) => true,
            Err(e) => {
                errors.push(format!("Invalid provider \"{p}\": {e}"));
                false
            }
        }
    } else {
        true // None means default (Anthropic), which is valid
    };

    // 5. Count setup commands for display
    let setup_command_count = run_cfg
        .as_ref()
        .and_then(|c| c.setup.as_ref())
        .map_or(0, |s| s.commands.len());

    // 6. Print structured report to stdout
    println!("workflow={}", graph.name);
    println!("nodes={}", graph.nodes.len());
    println!("edges={}", graph.edges.len());
    println!("goal={}", graph.goal());
    println!("sandbox={sandbox_provider}");
    println!("sandbox_ready={sandbox_ready}");
    println!("git_clean={git_clean}");
    println!("llm_available={llm_available}");
    println!("llm_providers={}", llm_providers.join(","));
    println!("model={model}");
    println!("provider={}", provider.as_deref().unwrap_or("anthropic"));
    println!("provider_valid={provider_valid}");
    println!("setup_commands={setup_command_count}");

    // 7. Print warnings/errors to stderr
    for err in &errors {
        eprintln!("{}: {err}", styles.red.apply_to("error"),);
    }

    // 8. Final verdict
    let ok = sandbox_ready && llm_available && provider_valid;
    if ok {
        eprintln!("\n{}", styles.bold_green.apply_to("Preflight: OK"),);
        Ok(())
    } else {
        eprintln!("\n{}", styles.bold_red.apply_to("Preflight: FAIL"),);
        std::process::exit(1);
    }
}

/// Generate a retro report for a completed workflow run.
///
/// Derives a basic retro from the checkpoint, then optionally runs the retro agent
/// for a richer narrative. Errors are logged as warnings rather than propagated.
#[allow(clippy::too_many_arguments)]
async fn generate_retro(
    run_id: &str,
    workflow_name: &str,
    goal: &str,
    logs_dir: &std::path::Path,
    failed: bool,
    failure_reason: Option<&str>,
    run_duration_ms: u64,
    dry_run_mode: bool,
    llm_client: Option<&arc_llm::client::Client>,
    sandbox: &Arc<dyn arc_agent::Sandbox>,
    provider_enum: Provider,
    model: &str,
    styles: &'static Styles,
) {
    let cp = match Checkpoint::load(&logs_dir.join("checkpoint.json")) {
        Ok(cp) => cp,
        Err(e) => {
            eprintln!(
                "{} Could not load checkpoint, skipping retro: {e}",
                styles.yellow.apply_to("Warning:"),
            );
            return;
        }
    };

    let stage_durations = crate::retro::extract_stage_durations(logs_dir);
    let mut retro = crate::retro::derive_retro(
        run_id,
        workflow_name,
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
                "{} Failed to save initial retro: {e}",
                styles.yellow.apply_to("Warning:"),
            );
        }
    }

    // Run retro agent session
    eprintln!("\n{}", styles.bold.apply_to("=== Retro ==="));
    eprintln!("{}", styles.dim.apply_to(format!("Running retro ({model})...")));
    let retro_start = std::time::Instant::now();
    let narrative_result = if dry_run_mode {
        Ok(crate::retro_agent::dry_run_narrative())
    } else if let Some(client) = llm_client {
        crate::retro_agent::run_retro_agent(sandbox, logs_dir, client, provider_enum, model).await
    } else {
        Err(anyhow::anyhow!("No LLM client available"))
    };
    let retro_dur = progress::format_duration_short(retro_start.elapsed());

    match narrative_result {
        Ok(narrative) => {
            retro.apply_narrative(narrative);
            match retro.save(logs_dir) {
                Ok(()) => {
                    // Line 1: smoothness + outcome with right-aligned duration
                    let smoothness_str = retro
                        .smoothness
                        .as_ref()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let outcome_str = retro
                        .outcome
                        .as_deref()
                        .unwrap_or("No outcome recorded");
                    let line1_content = format!("Retro: {smoothness_str} \u{2014} {outcome_str}");
                    let term_width = console::Term::stderr().size().1 as usize;
                    let dur_len = retro_dur.len();
                    let pad1 = term_width.saturating_sub(line1_content.len() + dur_len);
                    eprintln!(
                        "{} {}{:pad1$}{}",
                        styles.bold.apply_to("Retro:"),
                        styles.dim.apply_to(format!("{smoothness_str} \u{2014} {outcome_str}")),
                        "",
                        styles.dim.apply_to(&retro_dur),
                    );

                    // Line 2: friction + open items (only if non-zero)
                    let friction_count = retro
                        .friction_points
                        .as_ref()
                        .map(|v| v.len())
                        .unwrap_or(0);
                    let open_count =
                        retro.open_items.as_ref().map(|v| v.len()).unwrap_or(0);
                    if friction_count > 0 || open_count > 0 {
                        let mut parts = Vec::new();
                        if friction_count > 0 {
                            let noun = if friction_count == 1 {
                                "friction point"
                            } else {
                                "friction points"
                            };
                            parts.push(format!("{friction_count} {noun}"));
                        }
                        if open_count > 0 {
                            let noun = if open_count == 1 {
                                "open item"
                            } else {
                                "open items"
                            };
                            parts.push(format!("{open_count} {noun}"));
                        }
                        eprintln!("  {}", styles.dim.apply_to(parts.join(" \u{00b7} ")));
                    }

                    // Line 3: file path
                    let retro_path = format!(
                        "{}/retro.json",
                        super::tilde_path(logs_dir)
                    );
                    eprintln!(
                        "  {} {}",
                        styles.dim.apply_to("Retro saved to"),
                        styles.underline.apply_to(&retro_path),
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to save retro with narrative: {e}",
                        styles.yellow.apply_to("Warning:"),
                    );
                }
            }
        }
        Err(e) => {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!("Retro agent skipped: {e}")),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_for_anthropic() {
        assert_eq!(
            default_model_for_provider(Provider::Anthropic),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn default_model_for_openai() {
        assert_eq!(default_model_for_provider(Provider::OpenAi), "gpt-5.2");
    }

    #[test]
    fn default_model_for_gemini() {
        assert_eq!(
            default_model_for_provider(Provider::Gemini),
            "gemini-3.1-pro-preview"
        );
    }

    #[test]
    fn default_model_for_kimi() {
        assert_eq!(default_model_for_provider(Provider::Kimi), "kimi-k2.5");
    }

    #[test]
    fn default_model_for_zai() {
        assert_eq!(default_model_for_provider(Provider::Zai), "glm-4.7");
    }

    #[test]
    fn default_model_for_minimax() {
        assert_eq!(
            default_model_for_provider(Provider::Minimax),
            "minimax-m2.5"
        );
    }

    #[test]
    fn default_model_for_inception() {
        assert_eq!(default_model_for_provider(Provider::Inception), "mercury");
    }

    #[test]
    fn resolve_model_provider_defaults() {
        let graph = crate::graph::types::Graph::new("test");
        let defaults = RunDefaults::default();
        let (model, provider) = resolve_model_provider(None, None, None, &defaults, &graph);
        assert_eq!(model, "claude-opus-4-6");
        // Catalog resolves anthropic as the provider for claude-opus-4-6
        assert_eq!(provider, Some("anthropic".to_string()));
    }

    #[test]
    fn resolve_model_provider_cli_overrides_toml() {
        let graph = crate::graph::types::Graph::new("test");
        let defaults = RunDefaults::default();
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: "test".to_string(),
            graph: "test.dot".to_string(),
            directory: None,
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
            }),
            setup: None,
            sandbox: None,
            vars: None,
        };
        let (model, provider) = resolve_model_provider(
            Some("gpt-5.2"),
            Some("openai"),
            Some(&cfg),
            &defaults,
            &graph,
        );
        assert_eq!(model, "gpt-5.2");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_toml_overrides_graph() {
        use crate::graph::types::AttrValue;
        let mut graph = crate::graph::types::Graph::new("test");
        graph.attrs.insert(
            "default_model".to_string(),
            AttrValue::String("graph-model".to_string()),
        );
        graph.attrs.insert(
            "default_provider".to_string(),
            AttrValue::String("gemini".to_string()),
        );

        let defaults = RunDefaults::default();
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: "test".to_string(),
            graph: "test.dot".to_string(),
            directory: None,
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
            }),
            setup: None,
            sandbox: None,
            vars: None,
        };
        let (model, provider) = resolve_model_provider(None, None, Some(&cfg), &defaults, &graph);
        assert_eq!(model, "toml-model");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_graph_attrs_used_as_fallback() {
        use crate::graph::types::AttrValue;
        let mut graph = crate::graph::types::Graph::new("test");
        graph.attrs.insert(
            "default_model".to_string(),
            AttrValue::String("gpt-5.2".to_string()),
        );
        graph.attrs.insert(
            "default_provider".to_string(),
            AttrValue::String("openai".to_string()),
        );

        let defaults = RunDefaults::default();
        let (model, provider) = resolve_model_provider(None, None, None, &defaults, &graph);
        assert_eq!(model, "gpt-5.2");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_alias_expansion() {
        let graph = crate::graph::types::Graph::new("test");
        let defaults = RunDefaults::default();
        let (model, provider) = resolve_model_provider(Some("opus"), None, None, &defaults, &graph);
        assert_eq!(model, "claude-opus-4-6");
        assert_eq!(provider, Some("anthropic".to_string()));
    }

    #[test]
    fn resolve_model_provider_run_defaults_used() {
        let graph = crate::graph::types::Graph::new("test");
        let defaults = RunDefaults {
            llm: Some(run_config::LlmConfig {
                model: Some("default-model".to_string()),
                provider: Some("openai".to_string()),
            }),
            ..RunDefaults::default()
        };
        let (model, provider) = resolve_model_provider(None, None, None, &defaults, &graph);
        assert_eq!(model, "default-model");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_model_provider_toml_overrides_run_defaults() {
        let graph = crate::graph::types::Graph::new("test");
        let defaults = RunDefaults {
            llm: Some(run_config::LlmConfig {
                model: Some("default-model".to_string()),
                provider: Some("anthropic".to_string()),
            }),
            ..RunDefaults::default()
        };
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: "test".to_string(),
            graph: "test.dot".to_string(),
            directory: None,
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
            }),
            setup: None,
            sandbox: None,
            vars: None,
        };
        let (model, provider) = resolve_model_provider(None, None, Some(&cfg), &defaults, &graph);
        assert_eq!(model, "toml-model");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_preserve_sandbox_cli_wins() {
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: "test".into(),
            graph: "w.dot".into(),
            directory: None,
            llm: None,
            setup: None,
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: Some(false),
                daytona: None,
            }),
            vars: None,
        };
        let defaults = RunDefaults::default();
        assert!(resolve_preserve_sandbox(true, Some(&cfg), &defaults));
    }

    #[test]
    fn resolve_preserve_sandbox_toml_wins_over_defaults() {
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: "test".into(),
            graph: "w.dot".into(),
            directory: None,
            llm: None,
            setup: None,
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: Some(true),
                daytona: None,
            }),
            vars: None,
        };
        let defaults = RunDefaults {
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: Some(false),
                daytona: None,
            }),
            ..RunDefaults::default()
        };
        assert!(resolve_preserve_sandbox(false, Some(&cfg), &defaults));
    }

    #[test]
    fn resolve_preserve_sandbox_defaults_used() {
        let defaults = RunDefaults {
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: Some(true),
                daytona: None,
            }),
            ..RunDefaults::default()
        };
        assert!(resolve_preserve_sandbox(false, None, &defaults));
    }

    #[test]
    fn resolve_preserve_sandbox_defaults_to_false() {
        let defaults = RunDefaults::default();
        assert!(!resolve_preserve_sandbox(false, None, &defaults));
    }

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
