use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Context};
use chrono::{Local, Utc};
use fabro_agent::{DockerSandbox, DockerSandboxConfig, LocalSandbox, Sandbox};
use fabro_util::terminal::Styles;
use tracing::debug;

use super::{relative_path, tilde_path};
use crate::checkpoint::Checkpoint;
use crate::engine::{RunConfig, WorkflowRunEngine};
use crate::event::EventEmitter;
use crate::handler::default_registry;
use crate::interviewer::auto_approve::AutoApproveInterviewer;
use crate::interviewer::console::ConsoleInterviewer;
use crate::interviewer::Interviewer;
use crate::outcome::StageStatus;
use crate::validation::Severity;
use crate::workflow::WorkflowBuilder;

use fabro_llm::provider::Provider;

use super::backend::AgentApiBackend;
use super::cli_backend::{AgentCliBackend, BackendRouter};
use super::progress;
use super::run_config;
use super::run_config::{RunDefaults, WorkflowRunConfig};
use crate::devcontainer_bridge;
use indicatif::HumanDuration;
use std::time::Duration;

use super::{
    compute_stage_cost, format_cost, format_tokens_human, print_diagnostics, read_workflow_file,
    RunArgs, SandboxProvider,
};

/// Resolve goal from `--goal` string or `--goal-file` path.
fn resolve_cli_goal(
    goal: &Option<String>,
    goal_file: &Option<PathBuf>,
) -> anyhow::Result<Option<String>> {
    match (goal, goal_file) {
        (Some(g), _) => Ok(Some(g.clone())),
        (_, Some(path)) => {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read goal file: {}", path.display()))?;
            debug!(path = %path.display(), "Goal loaded from file");
            Ok(Some(content))
        }
        _ => Ok(None),
    }
}

/// Apply goal to the graph from TOML config or CLI flag.
/// Precedence: CLI `--goal` / `--goal-file` > TOML `goal` > DOT `graph [goal="..."]`.
fn apply_goal_override(
    graph: &mut crate::graph::types::Graph,
    cli_goal: Option<&str>,
    toml_goal: Option<&str>,
) {
    let goal = cli_goal.or(toml_goal);
    if let Some(goal) = goal {
        debug!(goal = %goal, "overriding graph goal");
        graph.attrs.insert(
            "goal".to_string(),
            crate::graph::types::AttrValue::String(goal.to_string()),
        );
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
            fabro_llm::catalog::default_model_for_provider(provider_enum.as_str())
                .map(|m| m.id)
                .unwrap_or_else(|| provider_enum.as_str().to_string())
        });

    // Resolve model alias through catalog
    match fabro_llm::catalog::get_model_info(&model) {
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

/// Resolve worktree mode: TOML config > run defaults > Clean.
fn resolve_worktree_mode(
    run_cfg: Option<&WorkflowRunConfig>,
    run_defaults: &RunDefaults,
) -> run_config::WorktreeMode {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|s| s.local.as_ref())
        .map(|l| l.worktree_mode)
        .unwrap_or_else(|| {
            run_defaults
                .sandbox
                .as_ref()
                .and_then(|s| s.local.as_ref())
                .map(|l| l.worktree_mode)
                .unwrap_or_default()
        })
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

#[cfg(feature = "exedev")]
/// Resolve exe.dev config: TOML config > run defaults.
fn resolve_exe_config(
    run_cfg: Option<&WorkflowRunConfig>,
    run_defaults: &RunDefaults,
) -> Option<fabro_exe::ExeConfig> {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|e| e.exe.clone())
        .or_else(|| run_defaults.sandbox.as_ref().and_then(|s| s.exe.clone()))
}

#[cfg(feature = "exedev")]
/// Resolve exe.dev git clone parameters from the current repo.
///
/// Returns `None` if no git repo is detected. Credential resolution is
/// handled by ExeSandbox itself via its `github_app` field.
fn resolve_exe_clone_params(cwd: &std::path::Path) -> Option<fabro_exe::GitCloneParams> {
    let (detected_url, branch) = match crate::daytona_sandbox::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("No git repo detected for exe.dev clone: {e}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_exe::GitCloneParams { url, branch })
}

/// Resolve SSH sandbox config: TOML config > run defaults.
fn resolve_ssh_config(
    run_cfg: Option<&WorkflowRunConfig>,
    run_defaults: &RunDefaults,
) -> Option<fabro_ssh::SshConfig> {
    run_cfg
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|e| e.ssh.clone())
        .or_else(|| run_defaults.sandbox.as_ref().and_then(|s| s.ssh.clone()))
}

/// Resolve SSH sandbox git clone parameters from the current repo.
///
/// Returns `None` if no git repo is detected. Credential resolution is
/// handled by SshSandbox itself via its `github_app` field.
fn resolve_ssh_clone_params(cwd: &std::path::Path) -> Option<fabro_ssh::GitCloneParams> {
    let (detected_url, branch) = match crate::daytona_sandbox::detect_repo_info(cwd) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("No git repo detected for SSH clone: {e}");
            return None;
        }
    };
    let url = fabro_github::ssh_url_to_https(&detected_url);
    Some(fabro_ssh::GitCloneParams { url, branch })
}

/// Resolve the fallback chain from config.
///
/// `apply_defaults` must be called on `run_cfg` before this — it merges
/// `run_defaults.llm.fallbacks` into `run_cfg.llm.fallbacks` already.
fn resolve_fallback_chain(
    provider: Provider,
    model: &str,
    run_cfg: Option<&WorkflowRunConfig>,
) -> Vec<fabro_llm::catalog::FallbackTarget> {
    let fallbacks = run_cfg
        .and_then(|c| c.llm.as_ref())
        .and_then(|l| l.fallbacks.as_ref());

    match fallbacks {
        Some(map) => fabro_llm::catalog::build_fallback_chain(provider.as_str(), model, map),
        None => Vec::new(),
    }
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
    mut run_defaults: RunDefaults,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: crate::git::GitAuthor,
) -> anyhow::Result<()> {
    // Handle --run-branch resume: read everything from git metadata
    if let Some(branch) = args.run_branch.clone() {
        return run_from_branch(args, &branch, styles, git_author, run_defaults, github_app).await;
    }

    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required unless --run-branch is provided"))?;

    // Apply project-level config overrides (fabro.toml) on top of CLI defaults.
    // Workflow-level config (workflow.toml) still wins via apply_defaults below.
    if let Ok(Some((_config_path, project_config))) =
        super::project_config::discover_project_config(&std::env::current_dir().unwrap_or_default())
    {
        if project_config.pull_request.is_some() {
            tracing::debug!(
                pull_request = ?project_config.pull_request,
                "Applying pull_request config from fabro.toml"
            );
            run_defaults.pull_request = project_config.pull_request;
        }
    }

    // 0. Resolve workflow arg, load run config if TOML, resolve DOT path, apply defaults
    let (dot_path, run_cfg) = {
        let (dot, cfg) = super::project_config::resolve_workflow(workflow_path)?;
        match cfg {
            Some(mut cfg) => {
                cfg.apply_defaults(&run_defaults);
                (dot, Some(cfg))
            }
            None => (dot, None),
        }
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
    let source = read_workflow_file(&dot_path)?;
    let vars = run_cfg
        .as_ref()
        .and_then(|c| c.vars.as_ref())
        .or(run_defaults.vars.as_ref());
    let source = match vars {
        Some(vars) => run_config::expand_vars(&source, vars)?,
        None => source,
    };
    let dot_dir = dot_path.parent().unwrap_or(std::path::Path::new("."));
    let (mut graph, diagnostics) =
        WorkflowBuilder::new().prepare_with_file_inlining(&source, dot_dir)?;
    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    let toml_goal = run_cfg.as_ref().and_then(|c| c.goal.as_deref());
    apply_goal_override(&mut graph, cli_goal.as_deref(), toml_goal);

    // Inline @file references in the (possibly overridden) goal
    if let Some(crate::graph::types::AttrValue::String(goal)) = graph.attrs.get("goal") {
        let fallback = dirs::home_dir().map(|h| h.join(".fabro"));
        let resolved = crate::transform::resolve_file_ref(goal, dot_dir, fallback.as_deref());
        if resolved != *goal {
            graph.attrs.insert(
                "goal".to_string(),
                crate::graph::types::AttrValue::String(resolved),
            );
        }
    }

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
    eprintln!(
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(relative_path(&dot_path)),
    );

    let goal = graph.goal();
    if !goal.is_empty() {
        let first_line = goal.lines().next().unwrap_or(goal);
        eprintln!("{} {first_line}\n", styles.bold.apply_to("Goal:"));
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
    let (origin_url, detected_base_branch) =
        crate::daytona_sandbox::detect_repo_info(&original_cwd)
            .map(|(url, branch)| (Some(url), branch))
            .unwrap_or((None, None));
    let git_clean = if sandbox_provider.is_remote() {
        crate::git::ensure_clean_and_pushed(
            &original_cwd,
            "origin",
            detected_base_branch.as_deref(),
        )
        .is_ok()
    } else {
        crate::git::ensure_clean(&original_cwd).is_ok()
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
            github_app,
            origin_url.as_deref(),
        )
        .await;
    }

    // 3. Create logs directory
    let run_id = ulid::Ulid::new().to_string();
    let run_dir = args.run_dir.unwrap_or_else(|| {
        let base = dirs::home_dir()
            .expect("could not determine home directory")
            .join(".fabro")
            .join("runs");
        base.join(format!("{}-{}", Local::now().format("%Y%m%d"), run_id))
    });
    tokio::fs::create_dir_all(&run_dir).await?;
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;
    tokio::fs::write(run_dir.join("graph.fabro"), &source).await?;
    tokio::fs::write(run_dir.join("run.pid"), std::process::id().to_string()).await?;
    if workflow_path.extension().is_some_and(|ext| ext == "toml") {
        if let Ok(toml_contents) = tokio::fs::read(workflow_path).await {
            tokio::fs::write(run_dir.join("run.toml"), toml_contents).await?;
        }
    }

    // Create progress UI (used for both normal and verbose modes)
    let is_tty = std::io::stderr().is_terminal();
    let progress_ui = Arc::new(Mutex::new(progress::ProgressUI::new(is_tty, args.verbose)));
    {
        let mut ui = progress_ui.lock().expect("progress lock poisoned");
        ui.show_version();
        ui.show_run_id(&run_id);
        ui.show_time(&Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
        ui.show_run_dir(&run_dir);
    }

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
        let jsonl_path = run_dir.join("progress.jsonl");
        let live_path = run_dir.join("live.json");
        let run_id = Arc::new(Mutex::new(String::new()));
        let run_id_clone = Arc::clone(&run_id);
        emitter.on_event(move |event| {
            if let crate::event::WorkflowRunEvent::WorkflowRunStarted { run_id, .. } = event {
                *run_id_clone.lock().unwrap() = run_id.clone();
            }
            let envelope = build_event_envelope(event, &run_id_clone.lock().unwrap());
            // Append to progress.jsonl
            if let Ok(line) = serde_json::to_string(&envelope) {
                let line = fabro_util::redact::redact_jsonl_line(&line);
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
                let pretty = fabro_util::redact::redact_jsonl_line(&pretty);
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

    // Set up git worktree for local execution (must happen before cwd is captured).
    // Remote sandboxes (Daytona, Exe) clone into their own environment, so a local
    // worktree is unnecessary and wastes disk.
    let worktree_mode = resolve_worktree_mode(run_cfg.as_ref(), &run_defaults);
    let should_create_worktree = if sandbox_provider.is_remote() {
        false
    } else {
        match worktree_mode {
            run_config::WorktreeMode::Always => true,
            run_config::WorktreeMode::Clean => git_clean,
            run_config::WorktreeMode::Dirty => !git_clean,
            run_config::WorktreeMode::Never => false,
        }
    };
    debug!(
        ?worktree_mode,
        ?sandbox_provider,
        git_clean,
        should_create_worktree,
        "Resolved worktree mode"
    );

    if should_create_worktree && !git_clean {
        eprintln!(
            "{} Uncommitted changes will not be included in the worktree.",
            styles.yellow.apply_to("Warning:"),
        );
    }

    if should_create_worktree {
        if let Some(ref branch) = detected_base_branch {
            let check_repo = original_cwd.clone();
            let check_branch = branch.clone();
            let needs_push = tokio::task::spawn_blocking(move || {
                crate::git::branch_needs_push(&check_repo, "origin", &check_branch)
            })
            .await
            .unwrap_or(true);

            if needs_push {
                let repo_path = original_cwd.clone();
                let branch_owned = branch.clone();
                let result = crate::git::blocking_push_with_timeout(60, move || {
                    crate::git::push_branch(&repo_path, "origin", &branch_owned)
                })
                .await;
                match result {
                    Ok(()) => {
                        tracing::info!(%branch, "Pushed current branch to origin");
                        eprintln!(
                            "{} {branch} (synced local commits to remote)",
                            styles.bold.apply_to("Pushed branch:")
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %branch, "Failed to push current branch");
                        eprintln!(
                            "{} Failed to push {branch} to origin: {e}",
                            styles.yellow.apply_to("Warning:")
                        );
                    }
                }
            } else {
                tracing::info!(%branch, "Branch already in sync with origin, skipping push");
            }
        }
    }

    let (worktree_work_dir, worktree_path, worktree_branch, worktree_base_sha) =
        if should_create_worktree {
            match setup_worktree(&original_cwd, &run_dir, &run_id) {
                Ok((wd, wt, branch, base)) => (Some(wd), Some(wt), Some(branch), Some(base)),
                Err(e) => {
                    eprintln!(
                        "{} Git worktree setup failed ({e}), running without worktree.",
                        styles.yellow.apply_to("Warning:"),
                    );
                    (None, None, None, None)
                }
            }
        } else {
            (None, None, None, None)
        };

    if let Some(ref wt) = worktree_path {
        progress_ui
            .lock()
            .expect("progress lock poisoned")
            .show_worktree(wt);
    }

    if let Some(ref sha) = worktree_base_sha {
        progress_ui
            .lock()
            .expect("progress lock poisoned")
            .show_base_info(detected_base_branch.as_deref(), sha);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut daytona_config = resolve_daytona_config(run_cfg.as_ref(), &run_defaults);
    #[cfg(feature = "exedev")]
    let exe_config = resolve_exe_config(run_cfg.as_ref(), &run_defaults);
    let ssh_config = resolve_ssh_config(run_cfg.as_ref(), &run_defaults);

    // Resolve devcontainer if enabled
    let devcontainer_config = if run_cfg
        .as_ref()
        .and_then(|c| c.sandbox.as_ref())
        .and_then(|s| s.devcontainer)
        .unwrap_or(false)
    {
        match fabro_devcontainer::DevcontainerResolver::resolve(&cwd).await {
            Ok(dc) => {
                let lifecycle_command_count = dc.on_create_commands.len()
                    + dc.post_create_commands.len()
                    + dc.post_start_commands.len();
                emitter.emit(&crate::event::WorkflowRunEvent::DevcontainerResolved {
                    dockerfile_lines: dc.dockerfile.lines().count(),
                    environment_count: dc.environment.len(),
                    lifecycle_command_count,
                    workspace_folder: dc.workspace_folder.clone(),
                });

                // Override daytona_config with devcontainer dockerfile
                let snapshot = devcontainer_bridge::devcontainer_to_snapshot_config(&dc);
                let mut cfg = daytona_config.unwrap_or_default();
                cfg.snapshot = Some(snapshot);
                daytona_config = Some(cfg);

                // Run initialize_commands on host
                let timeout = std::time::Duration::from_millis(300_000);
                for cmd in &dc.initialize_commands {
                    let shell_cmds = match cmd {
                        fabro_devcontainer::Command::Shell(s) => vec![s.clone()],
                        fabro_devcontainer::Command::Args(args) => {
                            vec![args
                                .iter()
                                .map(|a| {
                                    shlex::try_quote(a).unwrap_or_else(|_| a.into()).to_string()
                                })
                                .collect::<Vec<_>>()
                                .join(" ")]
                        }
                        fabro_devcontainer::Command::Parallel(map) => {
                            map.values().cloned().collect()
                        }
                    };
                    for shell_cmd in &shell_cmds {
                        let fut = tokio::process::Command::new("sh")
                            .arg("-c")
                            .arg(shell_cmd)
                            .current_dir(&cwd)
                            .output();
                        let output = tokio::time::timeout(timeout, fut)
                            .await
                            .with_context(|| {
                                format!("Devcontainer initializeCommand timed out: {shell_cmd}")
                            })?
                            .with_context(|| {
                                format!(
                                    "Failed to execute devcontainer initializeCommand: {shell_cmd}"
                                )
                            })?;
                        if !output.status.success() {
                            let code = output
                                .status
                                .code()
                                .map_or("unknown".to_string(), |c| c.to_string());
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            bail!(
                                "Devcontainer initializeCommand failed (exit code {code}): {shell_cmd}\n{stderr}"
                            );
                        }
                    }
                }

                Some(dc)
            }
            Err(e) => {
                bail!("Failed to resolve devcontainer: {e}");
            }
        }
    } else {
        None
    };

    // Wrap emitter in Fabro now so we can share it with exec env callbacks
    let emitter = Arc::new(emitter);

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
            let mut env = crate::daytona_sandbox::DaytonaSandbox::new(
                daytona_client,
                config,
                github_app.clone(),
                Some(run_id.clone()),
                detected_base_branch.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => {
            let clone_params = resolve_exe_clone_params(&original_cwd);

            let mgmt_ssh = fabro_exe::OpensshRunner::connect_raw("exe.dev")
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to exe.dev: {e}"))?;
            let config = exe_config.unwrap_or_default();
            let mut env = fabro_exe::ExeSandbox::new(
                Box::new(mgmt_ssh),
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Ssh => {
            let config = ssh_config
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--sandbox ssh requires [sandbox.ssh] config"))?;
            let clone_params = resolve_ssh_clone_params(&original_cwd);
            let mut env = fabro_ssh::SshSandbox::new(
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Local => {
            let mut env = LocalSandbox::new(cwd.clone());
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

    progress_ui
        .lock()
        .expect("progress lock poisoned")
        .set_working_directory(sandbox.working_directory().to_string());

    // Persist sandbox connection info for `fabro cp`
    {
        let sandbox_info_opt = {
            let info = sandbox.sandbox_info();
            if info.is_empty() {
                None
            } else {
                Some(info)
            }
        };
        let record = match sandbox_provider {
            SandboxProvider::Local => crate::sandbox_record::SandboxRecord {
                provider: "local".to_string(),
                working_directory: sandbox.working_directory().to_string(),
                identifier: None,
                host_working_directory: None,
                container_mount_point: None,
                data_host: None,
            },
            SandboxProvider::Docker => crate::sandbox_record::SandboxRecord {
                provider: "docker".to_string(),
                working_directory: sandbox.working_directory().to_string(),
                identifier: sandbox_info_opt,
                host_working_directory: Some(cwd.to_string_lossy().to_string()),
                container_mount_point: Some(sandbox.working_directory().to_string()),
                data_host: None,
            },
            SandboxProvider::Daytona => crate::sandbox_record::SandboxRecord {
                provider: "daytona".to_string(),
                working_directory: sandbox.working_directory().to_string(),
                identifier: sandbox_info_opt,
                host_working_directory: None,
                container_mount_point: None,
                data_host: None,
            },
            #[cfg(feature = "exedev")]
            SandboxProvider::Exe => {
                // Extract data_host from the ssh access command ("ssh <host>")
                let data_host = sandbox
                    .ssh_access_command()
                    .await
                    .ok()
                    .flatten()
                    .and_then(|cmd| cmd.strip_prefix("ssh ").map(String::from));
                crate::sandbox_record::SandboxRecord {
                    provider: "exe".to_string(),
                    working_directory: sandbox.working_directory().to_string(),
                    identifier: sandbox_info_opt,
                    host_working_directory: None,
                    container_mount_point: None,
                    data_host,
                }
            }
            SandboxProvider::Ssh => {
                let data_host = ssh_config.as_ref().map(|c| c.destination.clone());
                crate::sandbox_record::SandboxRecord {
                    provider: "ssh".to_string(),
                    working_directory: sandbox.working_directory().to_string(),
                    identifier: sandbox_info_opt,
                    host_working_directory: None,
                    container_mount_point: None,
                    data_host,
                }
            }
        };
        if let Err(e) = record.save(&run_dir.join("sandbox.json")) {
            tracing::warn!(error = %e, "Failed to save sandbox record");
        }
    }

    // Wrap with ReadBeforeWriteSandbox to enforce read-before-write guard
    let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::ReadBeforeWriteSandbox::new(sandbox));

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

    // Set up git inside remote sandbox (Daytona or exe.dev) for checkpoint commits
    let (remote_base_sha, remote_branch, remote_base_branch) = if sandbox.is_remote() {
        match setup_remote_git(&*sandbox, &run_id).await {
            Ok((base, branch, base_br)) => (Some(base), Some(branch), base_br),
            Err(e) => {
                eprintln!(
                    "{} Remote git setup failed ({e}), running without git checkpoints.",
                    styles.yellow.apply_to("Warning:"),
                );
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };

    if worktree_base_sha.is_none() {
        if let Some(ref sha) = remote_base_sha {
            let branch = detected_base_branch
                .as_deref()
                .or(remote_base_branch.as_deref());
            progress_ui
                .lock()
                .expect("progress lock poisoned")
                .show_base_info(branch, sha);
        }
    }

    // Create SSH access if requested
    if args.ssh {
        match sandbox.ssh_access_command().await {
            Ok(Some(ssh_command)) => {
                emitter.emit(&crate::event::WorkflowRunEvent::SshAccessReady { ssh_command });
            }
            Ok(None) => {
                eprintln!(
                    "{} --ssh only works with --sandbox daytona, exe, or ssh, skipping.",
                    styles.yellow.apply_to("Warning:"),
                );
            }
            Err(e) => {
                eprintln!(
                    "{} Failed to create SSH access: {e}",
                    styles.yellow.apply_to("Warning:"),
                );
            }
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
            let cmd_duration = crate::millis_u64(cmd_start.elapsed());
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
        let setup_duration = crate::millis_u64(setup_start.elapsed());
        emitter.emit(&crate::event::WorkflowRunEvent::SetupCompleted {
            duration_ms: setup_duration,
        });
    }

    // Run devcontainer lifecycle hooks inside the sandbox
    if let Some(ref dc) = devcontainer_config {
        let phases: &[(&str, &[fabro_devcontainer::Command])] = &[
            ("on_create", &dc.on_create_commands),
            ("post_create", &dc.post_create_commands),
            ("post_start", &dc.post_start_commands),
        ];
        for (phase, commands) in phases {
            devcontainer_bridge::run_devcontainer_lifecycle(
                sandbox.as_ref(),
                &emitter,
                phase,
                commands,
                300_000,
            )
            .await?;
        }
    }

    // 6. Resolve backend, model, and provider
    let (dry_run_mode, llm_client) = if args.dry_run {
        (true, None)
    } else {
        match fabro_llm::client::Client::from_env().await {
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

    // Resolve fallback chain from config
    let fallback_chain = resolve_fallback_chain(provider_enum, &model, run_cfg.as_ref());

    // 7. Build engine
    // Devcontainer env is layered underneath TOML env (TOML wins on conflict)
    let sandbox_env: HashMap<String, String> = {
        let mut env = if let Some(ref dc) = devcontainer_config {
            dc.environment.clone()
        } else {
            HashMap::new()
        };
        if let Some(toml_env) = run_cfg
            .as_ref()
            .and_then(|c| c.sandbox.as_ref())
            .and_then(|s| s.env.clone())
        {
            env.extend(toml_env);
        }
        env
    };
    let mcp_servers: Vec<fabro_mcp::config::McpServerConfig> = run_cfg
        .as_ref()
        .map(|c| {
            c.mcp_servers
                .clone()
                .into_iter()
                .map(|(name, entry)| entry.into_config(name))
                .collect()
        })
        .unwrap_or_default();
    let registry = default_registry(interviewer.clone(), {
        let sandbox_env = sandbox_env.clone();
        let model = model.clone();
        let mcp_servers = mcp_servers.clone();
        move || {
            if dry_run_mode {
                None
            } else {
                let api =
                    AgentApiBackend::new(model.clone(), provider_enum, fallback_chain.clone())
                        .with_env(sandbox_env.clone())
                        .with_mcp_servers(mcp_servers.clone());
                let cli = AgentCliBackend::new(model.clone(), provider_enum)
                    .with_env(sandbox_env.clone());
                Some(Box::new(BackendRouter::new(Box::new(api), cli)))
            }
        }
    });
    let mut engine = WorkflowRunEngine::with_interviewer(
        registry,
        Arc::clone(&emitter),
        interviewer,
        Arc::clone(&sandbox),
    );
    if !sandbox_env.is_empty() {
        engine.set_env(sandbox_env);
    }

    // Wire up hook runner from run config
    if let Some(ref cfg) = run_cfg {
        if !cfg.hooks.is_empty() {
            let hook_config = crate::hook::HookConfig {
                hooks: cfg.hooks.clone(),
            };
            let runner = crate::hook::HookRunner::new(hook_config);
            engine.set_hook_runner(Arc::new(runner));
        }
    }

    // 7. Execute
    // Set up metadata branch for git checkpointing (host or remote)
    let meta_branch = if worktree_work_dir.is_some() || remote_base_sha.is_some() {
        Some(crate::git::MetadataStore::branch_name(&run_id))
    } else {
        None
    };
    let checkpoint_exclude_globs = run_cfg
        .as_ref()
        .map(|c| c.checkpoint.exclude_globs.clone())
        .unwrap_or_default();
    let pr_cfg = run_cfg.as_ref().and_then(|c| c.pull_request.as_ref());
    let config = RunConfig {
        run_dir: run_dir.clone(),
        cancel_token: None,
        dry_run: dry_run_mode,
        run_id: run_id.clone(),
        git_checkpoint_enabled: if sandbox.is_remote() {
            remote_base_sha.is_some()
        } else {
            worktree_work_dir.is_some()
        },
        host_repo_path: Some(original_cwd.clone()),
        base_sha: worktree_base_sha.or(remote_base_sha),
        run_branch: worktree_branch.or(remote_branch),
        meta_branch,
        labels: args
            .label
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        checkpoint_exclude_globs,
        github_app: github_app.clone(),
        git_author,
        base_branch: detected_base_branch.or(remote_base_branch),
        pull_request_enabled: pr_cfg.is_some_and(|p| p.enabled),
        pull_request_draft: pr_cfg.is_none_or(|p| p.draft),
        asset_globs: run_cfg
            .as_ref()
            .and_then(|c| c.assets.as_ref())
            .map(|a| a.include.clone())
            .unwrap_or_default(),
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

    // Restore cwd (worktree is kept for `fabro cp` access; pruned separately)
    let _ = std::env::set_current_dir(&original_cwd);

    {
        let (status, failure_reason) = match &engine_result {
            Ok(o) => (o.status.clone(), o.failure_reason().map(String::from)),
            Err(e) => (crate::outcome::StageStatus::Fail, Some(e.to_string())),
        };

        // Load checkpoint and stage durations to populate per-stage data
        let checkpoint = Checkpoint::load(&run_dir.join("checkpoint.json")).ok();
        let stage_durations = crate::retro::extract_stage_durations(&run_dir);

        let (stages, total_cost, total_retries) = if let Some(ref cp) = checkpoint {
            let mut stages = Vec::new();
            let mut cost_sum: Option<f64> = None;
            let mut retries_sum: u32 = 0;

            for node_id in &cp.completed_nodes {
                let outcome = cp.node_outcomes.get(node_id);
                let retries = cp
                    .node_retries
                    .get(node_id)
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1);
                retries_sum += retries;

                let cost = outcome.and_then(|o| o.usage.as_ref()).and_then(|u| u.cost);
                if let Some(c) = cost {
                    *cost_sum.get_or_insert(0.0) += c;
                }

                stages.push(crate::conclusion::StageSummary {
                    stage_id: node_id.clone(),
                    stage_label: node_id.clone(),
                    duration_ms: stage_durations.get(node_id).copied().unwrap_or(0),
                    cost,
                    retries,
                });
            }
            (stages, cost_sum, retries_sum)
        } else {
            (vec![], None, 0)
        };

        let conclusion = crate::conclusion::Conclusion {
            timestamp: Utc::now(),
            status,
            duration_ms: run_duration_ms,
            failure_reason,
            final_git_commit_sha: last_git_sha.lock().unwrap().clone(),
            stages,
            total_cost,
            total_retries,
        };
        let _ = conclusion.save(&run_dir.join("conclusion.json"));
    }

    // Auto-derive retro (always, cheap) and optionally run retro agent
    if !args.no_retro && super::project_config::is_retro_enabled() {
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
            &run_dir,
            failed,
            failure_reason.as_deref(),
            run_duration_ms,
            dry_run_mode,
            llm_client.as_ref(),
            &sandbox,
            provider_enum,
            &model,
            styles,
            Some(&progress_ui),
        )
        .await;
    }

    // Finish progress bars after retro (retro stage uses the same ProgressUI)
    progress_ui.lock().expect("progress lock poisoned").finish();

    // Write finalize commit with retro.json + final node files (captures last diff.patch)
    write_finalize_commit(&config, &run_dir).await;

    // Auto-create PR on successful completion
    let mut pushed_branch: Option<String> = None;
    let mut pr_url: Option<String> = None;
    if config.pull_request_enabled {
        if let Ok(ref outcome) = engine_result {
            if matches!(
                outcome.status,
                StageStatus::Success | StageStatus::PartialSuccess
            ) {
                let diff = tokio::fs::read_to_string(run_dir.join("final.patch"))
                    .await
                    .unwrap_or_default();
                if let (
                    Some(ref base_branch),
                    Some(ref run_branch),
                    Some(ref creds),
                    Some(ref origin),
                ) = (
                    &config.base_branch,
                    &config.run_branch,
                    &github_app,
                    &origin_url,
                ) {
                    // Run branch was pushed during checkpoint commits;
                    // just record it for the PR creation.
                    if config.git_checkpoint_enabled {
                        pushed_branch = Some(run_branch.clone());
                    }

                    match crate::pull_request::maybe_open_pull_request(
                        creds,
                        origin,
                        base_branch,
                        run_branch,
                        graph.goal(),
                        &diff,
                        &model,
                        config.pull_request_draft,
                        &run_dir,
                    )
                    .await
                    {
                        Ok(Some(record)) => {
                            emitter.emit(&crate::event::WorkflowRunEvent::PullRequestCreated {
                                pr_url: record.html_url.clone(),
                                pr_number: record.number,
                                draft: config.pull_request_draft,
                            });
                            pr_url = Some(record.html_url.clone());
                            if let Err(e) = record.save(&run_dir.join("pull_request.json")) {
                                tracing::warn!(error = %e, "Failed to save pull_request.json");
                            }
                        }
                        Ok(None) => {} // empty diff, logged at DEBUG
                        Err(e) => {
                            emitter.emit(&crate::event::WorkflowRunEvent::PullRequestFailed {
                                error: e.to_string(),
                            });
                            eprintln!(
                                "{} PR creation failed: {e}",
                                styles.yellow.apply_to("Warning:")
                            );
                        }
                    }
                }
            }
        }
    }

    let outcome = engine_result?;

    // 8. Print result
    eprintln!("\n{}", styles.bold.apply_to("=== Run Result ==="),);

    eprintln!("{}", styles.dim.apply_to(format!("Run:       {run_id}")));
    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => &styles.bold_green,
        _ => &styles.bold_red,
    };
    eprintln!("Status:    {}", status_color.apply_to(&status_str),);
    eprintln!(
        "Duration:  {}",
        HumanDuration(Duration::from_millis(run_duration_ms))
    );

    {
        let acc = accumulator.lock().unwrap();
        let total_tokens = acc.total_input_tokens + acc.total_output_tokens;
        if total_tokens > 0 {
            if acc.has_pricing {
                eprintln!(
                    "{}",
                    styles.dim.apply_to(format!(
                        "Cost:      {} ({} toks)",
                        format_cost(acc.total_cost),
                        format_tokens_human(total_tokens)
                    ))
                );
            } else {
                eprintln!(
                    "{}",
                    styles
                        .dim
                        .apply_to(format!("Toks:      {}", format_tokens_human(total_tokens)))
                );
            }
            if acc.total_cache_read_tokens > 0 {
                eprintln!(
                    "{}",
                    styles.dim.apply_to(format!(
                        "Cache:     {} read, {} write",
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
    }

    eprintln!(
        "{}",
        styles
            .dim
            .apply_to(format!("Run:       {}", tilde_path(&run_dir)))
    );

    if let Some(failure) = outcome.failure_reason() {
        eprintln!("Failure:   {}", styles.red.apply_to(failure));
    }

    if pushed_branch.is_some() || pr_url.is_some() {
        eprintln!();
        if let Some(ref branch) = pushed_branch {
            eprintln!("{} {branch}", styles.bold.apply_to("Pushed branch:"));
        }
        if let Some(ref url) = pr_url {
            eprintln!("{} {url}", styles.bold.apply_to("Pull request:"));
        }
    }

    print_final_output(&run_dir, styles);
    print_assets(&run_dir, styles);

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
            eprintln!("\n{} sandbox preserved", styles.bold.apply_to("Info:"));
        }
    } else if let Err(e) = sandbox.cleanup().await {
        tracing::warn!(error = %e, "Sandbox cleanup failed");
        eprintln!(
            "\n{} sandbox cleanup failed: {e}",
            styles.yellow.apply_to("Warning:")
        );
    }

    // 10. Exit code
    fabro_util::run_log::deactivate();
    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => {
            std::process::exit(1);
        }
    }
}

/// Set up a git worktree for an isolated workflow run.
/// Caller must have already verified the repo is clean via `git::ensure_clean`.
/// Returns (work_dir, worktree_path, branch_name, base_sha) on success.
fn setup_worktree(
    original_cwd: &std::path::Path,
    run_dir: &std::path::Path,
    run_id: &str,
) -> anyhow::Result<(PathBuf, PathBuf, String, String)> {
    let base_sha = crate::git::head_sha(original_cwd).map_err(|e| anyhow::anyhow!("{e}"))?;
    let branch_name = format!("{}{run_id}", crate::git::RUN_BRANCH_PREFIX);
    crate::git::create_branch(original_cwd, &branch_name).map_err(|e| anyhow::anyhow!("{e}"))?;

    let worktree_path = run_dir.join("worktree");
    crate::git::replace_worktree(original_cwd, &worktree_path, &branch_name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    std::env::set_current_dir(&worktree_path)?;

    Ok((worktree_path.clone(), worktree_path, branch_name, base_sha))
}

/// Set up git inside a remote sandbox (Daytona or exe.dev) for checkpoint commits.
/// Returns (base_sha, branch_name, base_branch) on success.
async fn setup_remote_git(
    sandbox: &dyn fabro_agent::Sandbox,
    run_id: &str,
) -> anyhow::Result<(String, String, Option<String>)> {
    // Get current branch name before creating the run branch
    let branch_result = sandbox
        .exec_command("git rev-parse --abbrev-ref HEAD", 10_000, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("git rev-parse --abbrev-ref HEAD failed: {e}"))?;
    let base_branch = if branch_result.exit_code == 0 {
        let name = branch_result.stdout.trim().to_string();
        if name.is_empty() || name == "HEAD" {
            None
        } else {
            Some(name)
        }
    } else {
        None
    };

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

    let branch_name = format!("{}{run_id}", crate::git::RUN_BRANCH_PREFIX);

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

    Ok((base_sha, branch_name, base_branch))
}

/// Resume a workflow run from a git run branch.
///
/// Reads the checkpoint, manifest, and graph DOT from the metadata branch
/// (`refs/fabro/{run_id}`), re-attaches a worktree to the existing run branch,
/// and resumes execution via `run_from_checkpoint()`.
async fn run_from_branch(
    args: RunArgs,
    run_branch: &str,
    styles: &'static Styles,
    git_author: crate::git::GitAuthor,
    run_defaults: RunDefaults,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> anyhow::Result<()> {
    // Extract run_id from branch name: "fabro/run/{run_id}" -> "{run_id}"
    let run_id = run_branch
        .strip_prefix(crate::git::RUN_BRANCH_PREFIX)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "invalid run branch format: expected '{}<run_id>', got '{run_branch}'",
                crate::git::RUN_BRANCH_PREFIX,
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
    let source =
        crate::git::MetadataStore::read_graph_dot(&original_cwd, &run_id)?.ok_or_else(|| {
            anyhow::anyhow!("no graph.fabro found on metadata branch for run {run_id}")
        })?;

    // If --pipeline was also provided, use it instead (allows overriding)
    let (mut graph, diagnostics) = if let Some(ref workflow_path) = args.workflow {
        crate::workflow::prepare_from_file(workflow_path)?
    } else {
        crate::workflow::WorkflowBuilder::new().prepare(&source)?
    };
    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    apply_goal_override(&mut graph, cli_goal.as_deref(), None);

    eprintln!(
        "{} {} from branch {} ({})",
        styles.bold.apply_to("Resuming workflow:"),
        graph.name,
        styles.dim.apply_to(run_branch),
        run_id,
    );

    super::print_diagnostics(&diagnostics, styles);
    if diagnostics
        .iter()
        .any(|d| d.severity == crate::validation::Severity::Error)
    {
        anyhow::bail!("Validation failed");
    }

    // Set up logs directory
    let run_dir = args.run_dir.unwrap_or_else(|| {
        let base = dirs::home_dir()
            .expect("could not determine home directory")
            .join(".fabro")
            .join("runs");
        base.join(format!(
            "{}-{}",
            chrono::Local::now().format("%Y%m%d"),
            run_id
        ))
    });
    tokio::fs::create_dir_all(&run_dir).await?;
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;
    tokio::fs::write(run_dir.join("graph.fabro"), &source).await?;

    let base_sha =
        crate::git::MetadataStore::read_manifest(&original_cwd, &run_id)?.and_then(|m| m.base_sha);

    // Resolve sandbox provider
    let sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(args.sandbox, None, &run_defaults)?
    };

    let emitter = Arc::new(EventEmitter::new());
    let (sandbox, worktree_path): (Arc<dyn fabro_agent::Sandbox>, Option<PathBuf>) =
        match sandbox_provider {
            SandboxProvider::Local | SandboxProvider::Docker => {
                // Re-attach worktree to the existing run branch
                let wt = run_dir.join("worktree");
                crate::git::replace_worktree(&original_cwd, &wt, run_branch).map_err(|e| {
                    anyhow::anyhow!("failed to attach worktree to {run_branch}: {e}")
                })?;
                std::env::set_current_dir(&wt)?;
                let mut env = fabro_agent::LocalSandbox::new(wt.clone());
                let emitter_cb = Arc::clone(&emitter);
                env.set_event_callback(Arc::new(move |event| {
                    emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
                }));
                (Arc::new(env), Some(wt))
            }
            #[cfg(feature = "exedev")]
            SandboxProvider::Exe => {
                let exe_config = resolve_exe_config(None, &run_defaults);
                let clone_params = resolve_exe_clone_params(&original_cwd);
                let mgmt_ssh = fabro_exe::OpensshRunner::connect_raw("exe.dev")
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to connect to exe.dev: {e}"))?;
                let config = exe_config.unwrap_or_default();
                let mut env = fabro_exe::ExeSandbox::new(
                    Box::new(mgmt_ssh),
                    config,
                    clone_params,
                    Some(run_id.clone()),
                    github_app.clone(),
                );
                let emitter_cb = Arc::clone(&emitter);
                env.set_event_callback(Arc::new(move |event| {
                    emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
                }));
                (Arc::new(env), None)
            }
            SandboxProvider::Ssh => {
                let config = resolve_ssh_config(None, &run_defaults).ok_or_else(|| {
                    anyhow::anyhow!("--sandbox ssh requires [sandbox.ssh] config")
                })?;
                let clone_params = resolve_ssh_clone_params(&original_cwd);
                let mut env = fabro_ssh::SshSandbox::new(
                    config,
                    clone_params,
                    Some(run_id.clone()),
                    github_app.clone(),
                );
                let emitter_cb = Arc::clone(&emitter);
                env.set_event_callback(Arc::new(move |event| {
                    emitter_cb.emit(&crate::event::WorkflowRunEvent::Sandbox { event });
                }));
                (Arc::new(env), None)
            }
            SandboxProvider::Daytona => {
                bail!("--run-branch resume is not yet supported with --sandbox daytona");
            }
        };

    // Initialize remote sandboxes and checkout the run branch
    if sandbox.is_remote() {
        sandbox
            .initialize()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize sandbox: {e}"))?;

        // Fetch and checkout the run branch inside the sandbox
        let fetch_cmd = format!("git fetch origin {run_branch} && git checkout {run_branch}");
        let result = sandbox
            .exec_command(&fetch_cmd, 60_000, None, None, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to checkout run branch in sandbox: {e}"))?;
        if result.exit_code != 0 {
            bail!(
                "Failed to checkout run branch in sandbox (exit {}): {}",
                result.exit_code,
                result.stderr
            );
        }
    }

    // Wrap with ReadBeforeWriteSandbox to enforce read-before-write guard
    let sandbox: Arc<dyn fabro_agent::Sandbox> =
        Arc::new(fabro_agent::ReadBeforeWriteSandbox::new(sandbox));

    // Build interviewer
    let interviewer: Arc<dyn crate::interviewer::Interviewer> = if args.auto_approve {
        Arc::new(crate::interviewer::auto_approve::AutoApproveInterviewer)
    } else {
        Arc::new(crate::interviewer::console::ConsoleInterviewer::new(styles))
    };

    // Build engine with a backend
    let dry_run_mode = args.dry_run
        || fabro_llm::client::Client::from_env()
            .await
            .map(|c| c.provider_names().is_empty())
            .unwrap_or(true);

    let model = args.model.unwrap_or_else(|| "claude-opus-4-6".to_string());
    let provider_enum = args
        .provider
        .as_deref()
        .map(|s| s.parse::<fabro_llm::provider::Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or(fabro_llm::provider::Provider::Anthropic);

    // No fallback config available for branch resume; use empty chain.
    let fallback_chain = Vec::new();

    let registry = crate::handler::default_registry(interviewer.clone(), || {
        if dry_run_mode {
            None
        } else {
            let api = AgentApiBackend::new(model.clone(), provider_enum, fallback_chain.clone());
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
        run_dir: run_dir.clone(),
        cancel_token: None,
        dry_run: dry_run_mode,
        run_id: run_id.clone(),
        git_checkpoint_enabled: if sandbox.is_remote() {
            true
        } else {
            worktree_path.is_some()
        },
        host_repo_path: Some(original_cwd.clone()),
        base_sha,
        run_branch: Some(run_branch.to_string()),
        meta_branch,
        labels: HashMap::new(),
        checkpoint_exclude_globs: Vec::new(),
        github_app: github_app.clone(),
        git_author,
        base_branch: None,
        pull_request_enabled: false,
        pull_request_draft: false,
        asset_globs: Vec::new(),
    };

    let run_start = Instant::now();
    let engine_result = engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await;
    let run_duration_ms = run_start.elapsed().as_millis() as u64;

    // Restore cwd (worktree is kept for `fabro cp` access; pruned separately)
    let _ = std::env::set_current_dir(&original_cwd);
    let _ = sandbox.cleanup().await;

    // Auto-derive retro
    if !args.no_retro && super::project_config::is_retro_enabled() {
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
            fabro_llm::client::Client::from_env().await.ok()
        };

        generate_retro(
            &config.run_id,
            &graph.name,
            graph.goal(),
            &run_dir,
            failed,
            failure_reason.as_deref(),
            run_duration_ms,
            dry_run_mode,
            llm_client.as_ref(),
            &sandbox,
            provider_enum,
            &model,
            styles,
            None,
        )
        .await;
    }

    // Write finalize commit with retro.json + final node files (captures last diff.patch)
    write_finalize_commit(&config, &run_dir).await;

    let outcome = engine_result?;

    eprintln!("\n{}", styles.bold.apply_to("=== Run Result ==="),);
    eprintln!("{}", styles.dim.apply_to(format!("Run:       {run_id}")));
    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => &styles.bold_green,
        _ => &styles.bold_red,
    };
    eprintln!("Status:    {}", status_color.apply_to(&status_str),);
    eprintln!(
        "Duration:  {}",
        HumanDuration(Duration::from_millis(run_duration_ms))
    );
    eprintln!(
        "{}",
        styles
            .dim
            .apply_to(format!("Run:       {}", tilde_path(&run_dir)))
    );

    print_final_output(&run_dir, styles);
    print_assets(&run_dir, styles);

    fabro_util::run_log::deactivate();
    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => std::process::exit(1),
    }
}

/// Print the final stage output from the checkpoint, if available.
fn print_final_output(run_dir: &std::path::Path, styles: &Styles) {
    let Ok(checkpoint) = Checkpoint::load(&run_dir.join("checkpoint.json")) else {
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
                eprintln!("{}", styles.render_markdown(text));
            }
            return;
        }
    }
}

/// Print collected asset paths, if any.
fn print_assets(run_dir: &std::path::Path, styles: &Styles) {
    let paths = crate::asset_snapshot::collect_asset_paths(run_dir);
    if paths.is_empty() {
        return;
    }
    let home = dirs::home_dir();
    eprintln!("\n{}", styles.bold.apply_to("=== Assets ==="));
    for path in &paths {
        let display = match &home {
            Some(home_dir) => {
                let home_str = home_dir.to_string_lossy();
                if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
                    format!("~{rest}")
                } else {
                    path.clone()
                }
            }
            None => path.clone(),
        };
        eprintln!("{display}");
    }
}

/// Validate run configuration without executing the workflow.
///
/// Boots the sandbox (init + cleanup), checks LLM provider availability,
/// resolves the model/provider through the full precedence chain, and prints
/// a styled check report.
#[allow(clippy::too_many_arguments)]
async fn run_preflight(
    graph: &crate::graph::types::Graph,
    run_cfg: &Option<run_config::WorkflowRunConfig>,
    args: &RunArgs,
    run_defaults: &RunDefaults,
    git_clean: bool,
    sandbox_provider: SandboxProvider,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    origin_url: Option<&str>,
) -> anyhow::Result<()> {
    use fabro_util::check_report::{
        CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus,
    };

    let mut checks: Vec<CheckResult> = Vec::new();

    // 1. Repository metadata
    let setup_command_count = run_cfg
        .as_ref()
        .and_then(|c| c.setup.as_ref())
        .map_or(0, |s| s.commands.len());

    let repo_summary = origin_url
        .map(|url| {
            let https = fabro_github::ssh_url_to_https(url);
            fabro_github::parse_github_owner_repo(&https)
                .map(|(owner, repo)| format!("{owner}/{repo}"))
                .unwrap_or_else(|_| url.to_string())
        })
        .unwrap_or_else(|| "unknown".into());

    checks.push(CheckResult {
        name: "Repository".into(),
        status: CheckStatus::Pass,
        summary: repo_summary,
        details: vec![
            CheckDetail::new(format!("Setup commands: {setup_command_count}")),
            CheckDetail {
                text: format!("Git clean: {git_clean}"),
                warn: !git_clean,
            },
        ],
        remediation: None,
    });

    // 2. Workflow metadata
    let (model, provider) = resolve_model_provider(
        args.model.as_deref(),
        args.provider.as_deref(),
        run_cfg.as_ref(),
        run_defaults,
        graph,
    );

    checks.push(CheckResult {
        name: "Workflow".into(),
        status: CheckStatus::Pass,
        summary: graph.name.clone(),
        details: vec![
            CheckDetail::new(format!("Nodes: {}", graph.nodes.len())),
            CheckDetail::new(format!("Edges: {}", graph.edges.len())),
            CheckDetail::new(format!("Goal: {}", graph.goal())),
        ],
        remediation: None,
    });

    // 2. Sandbox boot check
    let original_cwd = std::env::current_dir()?;
    let daytona_config = resolve_daytona_config(run_cfg.as_ref(), run_defaults);
    #[cfg(feature = "exedev")]
    let exe_config = resolve_exe_config(run_cfg.as_ref(), run_defaults);
    let ssh_config = resolve_ssh_config(run_cfg.as_ref(), run_defaults);

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
                let env = crate::daytona_sandbox::DaytonaSandbox::new(
                    daytona_client,
                    config,
                    github_app,
                    None,
                    None,
                );
                Ok(Arc::new(env) as Arc<dyn Sandbox>)
            }
            Err(e) => Err(format!("Daytona client creation failed: {e}")),
        },
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => match fabro_exe::OpensshRunner::connect_raw("exe.dev").await {
            Ok(mgmt_ssh) => {
                let config = exe_config.unwrap_or_default();
                let clone_params = resolve_exe_clone_params(&original_cwd);
                let env = fabro_exe::ExeSandbox::new(
                    Box::new(mgmt_ssh),
                    config,
                    clone_params,
                    None,
                    None,
                );
                Ok(Arc::new(env) as Arc<dyn Sandbox>)
            }
            Err(e) => Err(format!("exe.dev SSH connection failed: {e}")),
        },
        SandboxProvider::Ssh => match ssh_config {
            Some(config) => {
                let clone_params = resolve_ssh_clone_params(&original_cwd);
                let env = fabro_ssh::SshSandbox::new(config, clone_params, None, None);
                Ok(Arc::new(env) as Arc<dyn Sandbox>)
            }
            None => Err("SSH sandbox requires [sandbox.ssh] config".to_string()),
        },
        SandboxProvider::Local => {
            Ok(Arc::new(LocalSandbox::new(original_cwd.clone())) as Arc<dyn Sandbox>)
        }
    };

    let sandbox_ok = match sandbox_result {
        Ok(sandbox) => match sandbox.initialize().await {
            Ok(()) => {
                let _ = sandbox.cleanup().await;
                true
            }
            Err(e) => {
                let _ = sandbox.cleanup().await;
                checks.push(CheckResult {
                    name: "Sandbox".into(),
                    status: CheckStatus::Error,
                    summary: "failed".into(),
                    details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                    remediation: Some(format!("Sandbox init failed: {e}")),
                });
                false
            }
        },
        Err(e) => {
            checks.push(CheckResult {
                name: "Sandbox".into(),
                status: CheckStatus::Error,
                summary: "failed".into(),
                details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
                remediation: Some(e),
            });
            false
        }
    };

    if sandbox_ok {
        checks.push(CheckResult {
            name: "Sandbox".into(),
            status: CheckStatus::Pass,
            summary: sandbox_provider.to_string(),
            details: vec![CheckDetail::new(format!("Provider: {sandbox_provider}"))],
            remediation: None,
        });
    }

    // 4. Per-model LLM checks
    let default_provider = provider.as_deref().unwrap_or("anthropic");
    let llm_ok = match fabro_llm::client::Client::from_env().await {
        Ok(c) => {
            let configured: Vec<String> =
                c.provider_names().iter().map(|s| s.to_string()).collect();

            // Collect all distinct (model, provider) pairs from LLM nodes
            let mut model_providers = std::collections::BTreeSet::new();
            for node in graph.nodes.values() {
                if !crate::graph::types::is_llm_handler_type(node.handler_type()) {
                    continue;
                }
                let node_model = node.model().unwrap_or(&model);
                let node_provider = node.provider().unwrap_or(default_provider);

                // Resolve through catalog to get canonical model ID and provider
                let (resolved_model, resolved_provider) =
                    if let Some(info) = fabro_llm::catalog::get_model_info(node_model) {
                        (info.id, info.provider)
                    } else {
                        (node_model.to_string(), node_provider.to_string())
                    };

                // Use node-level provider override if explicitly set, otherwise catalog provider
                let final_provider = if node.provider().is_some() {
                    node_provider.to_string()
                } else {
                    resolved_provider
                };

                model_providers.insert((resolved_model, final_provider));
            }

            // If no LLM nodes found, fall back to the default model/provider
            if model_providers.is_empty() {
                let (resolved_model, resolved_provider) =
                    if let Some(info) = fabro_llm::catalog::get_model_info(&model) {
                        (info.id, info.provider)
                    } else {
                        (model.clone(), default_provider.to_string())
                    };
                model_providers.insert((resolved_model, resolved_provider));
            }

            let mut all_ok = true;
            for (model_id, provider_name) in &model_providers {
                match provider_name.parse::<Provider>() {
                    Ok(_) => {
                        let mut status = CheckStatus::Pass;
                        if !configured.iter().any(|n| n == provider_name) {
                            status = CheckStatus::Warning;
                            all_ok = false;
                        }
                        checks.push(CheckResult {
                            name: "LLM".into(),
                            status,
                            summary: model_id.clone(),
                            details: vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                            remediation: if status == CheckStatus::Warning {
                                Some(format!("Provider \"{provider_name}\" is not configured"))
                            } else {
                                None
                            },
                        });
                    }
                    Err(e) => {
                        checks.push(CheckResult {
                            name: "LLM".into(),
                            status: CheckStatus::Error,
                            summary: model_id.clone(),
                            details: vec![CheckDetail::new(format!("Provider: {provider_name}"))],
                            remediation: Some(format!("Invalid provider \"{provider_name}\": {e}")),
                        });
                        all_ok = false;
                    }
                }
            }
            all_ok
        }
        Err(e) => {
            checks.push(CheckResult {
                name: "LLM".into(),
                status: CheckStatus::Error,
                summary: "initialization failed".into(),
                details: vec![],
                remediation: Some(format!("LLM client init failed: {e}")),
            });
            false
        }
    };

    // 5. Render report
    let report = CheckReport {
        title: "Run Preflight".into(),
        sections: vec![CheckSection {
            title: String::new(),
            checks,
        }],
    };

    let term_width = console::Term::stderr().size().1;
    print!("{}", report.render(styles, true, None, Some(term_width)));

    if sandbox_ok && llm_ok {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// Write a finalize commit to the shadow branch with retro.json and final node files.
///
/// This captures the last diff.patch (written after the final checkpoint) and retro.json.
/// Best-effort: errors are logged as warnings.
async fn write_finalize_commit(config: &RunConfig, run_dir: &std::path::Path) {
    let (Some(ref meta_branch), Some(ref repo_path)) =
        (&config.meta_branch, &config.host_repo_path)
    else {
        return;
    };

    let store = crate::git::MetadataStore::new(repo_path, &config.git_author);
    let mut entries = crate::git::scan_node_files(run_dir);
    if let Ok(retro_bytes) = std::fs::read(run_dir.join("retro.json")) {
        entries.push(("retro.json".to_string(), retro_bytes));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    if let Err(e) = store.write_files(&config.run_id, &refs, "finalize run") {
        tracing::warn!(error = %e, "Failed to write finalize commit to metadata branch");
        return;
    }

    // Push the finalize commit
    let run_id_part = meta_branch
        .strip_prefix("refs/fabro/")
        .unwrap_or(meta_branch);
    let refspec = format!("{meta_branch}:refs/heads/fabro/meta/{run_id_part}");
    crate::engine::git_push_host(repo_path, &refspec, &config.github_app, "finalize metadata")
        .await;
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
    run_dir: &std::path::Path,
    failed: bool,
    failure_reason: Option<&str>,
    run_duration_ms: u64,
    dry_run_mode: bool,
    llm_client: Option<&fabro_llm::client::Client>,
    sandbox: &Arc<dyn fabro_agent::Sandbox>,
    provider_enum: Provider,
    model: &str,
    styles: &'static Styles,
    progress_ui: Option<&Arc<Mutex<progress::ProgressUI>>>,
) {
    let cp = match Checkpoint::load(&run_dir.join("checkpoint.json")) {
        Ok(cp) => cp,
        Err(e) => {
            eprintln!(
                "{} Could not load checkpoint, skipping retro: {e}",
                styles.yellow.apply_to("Warning:"),
            );
            return;
        }
    };

    let stage_durations = crate::retro::extract_stage_durations(run_dir);
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

    match retro.save(run_dir) {
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

    let retro_start = std::time::Instant::now();
    let emitter = if let Some(pui) = progress_ui {
        let mut em = EventEmitter::new();
        progress::ProgressUI::register(pui, &mut em);
        let em = Arc::new(em);
        em.emit(&crate::event::WorkflowRunEvent::StageStarted {
            node_id: "retro".to_string(),
            name: "Retro".to_string(),
            index: 0,
            handler_type: Some("agent".to_string()),
            script: None,
            attempt: 1,
            max_attempts: 1,
        });
        Some(em)
    } else {
        eprintln!(
            "{}",
            styles.dim.apply_to(format!("Running retro ({model})..."))
        );
        None
    };

    let narrative_result = if dry_run_mode {
        Ok(crate::retro_agent::dry_run_narrative())
    } else if let Some(client) = llm_client {
        crate::retro_agent::run_retro_agent(
            sandbox,
            run_dir,
            client,
            provider_enum,
            model,
            emitter.clone(),
        )
        .await
    } else {
        Err(anyhow::anyhow!("No LLM client available"))
    };
    let retro_dur_elapsed = retro_start.elapsed();

    if let Some(ref em) = emitter {
        let status = if narrative_result.is_ok() {
            "success"
        } else {
            "fail"
        };
        em.emit(&crate::event::WorkflowRunEvent::StageCompleted {
            node_id: "retro".to_string(),
            name: "Retro".to_string(),
            index: 0,
            duration_ms: retro_dur_elapsed.as_millis() as u64,
            status: status.to_string(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure: None,
            notes: None,
            files_touched: vec![],
            attempt: 1,
            max_attempts: 1,
        });
    }

    let retro_dur = progress::format_duration_short(retro_dur_elapsed);

    match narrative_result {
        Ok(narrative) => {
            retro.apply_narrative(narrative);
            match retro.save(run_dir) {
                Ok(()) => {
                    // Line 1: smoothness + outcome with right-aligned duration
                    let smoothness_str = retro
                        .smoothness
                        .as_ref()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let outcome_str = retro.outcome.as_deref().unwrap_or("No outcome recorded");
                    let line1_content = format!("Retro: {smoothness_str} \u{2014} {outcome_str}");
                    let term_width = console::Term::stderr().size().1 as usize;
                    let dur_len = retro_dur.len();
                    let pad1 = term_width.saturating_sub(line1_content.len() + dur_len);
                    eprintln!(
                        "{} {}{:pad1$}{}",
                        styles.bold.apply_to("Retro:"),
                        styles
                            .dim
                            .apply_to(format!("{smoothness_str} \u{2014} {outcome_str}")),
                        "",
                        styles.dim.apply_to(&retro_dur),
                    );

                    // Line 2: friction + open items (only if non-zero)
                    let friction_count =
                        retro.friction_points.as_ref().map(|v| v.len()).unwrap_or(0);
                    let open_count = retro.open_items.as_ref().map(|v| v.len()).unwrap_or(0);
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
                    let retro_path = format!("{}/retro.json", super::tilde_path(run_dir));
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

fn build_event_envelope(event: &crate::event::WorkflowRunEvent, run_id: &str) -> serde_json::Value {
    let (event_name, event_fields) = crate::event::flatten_event(event);
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "ts".to_string(),
        serde_json::Value::String(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    envelope.insert(
        "run_id".to_string(),
        serde_json::Value::String(run_id.to_string()),
    );
    envelope.insert("event".to_string(), serde_json::Value::String(event_name));
    for (k, v) in event_fields {
        if k != "ts" && k != "run_id" && k != "event" {
            envelope.insert(k, v);
        }
    }
    serde_json::Value::Object(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_goal_override_cli_wins_over_toml() {
        use crate::graph::types::{AttrValue, Graph};
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("original".to_string()),
        );
        apply_goal_override(&mut graph, Some("CLI goal"), Some("TOML goal"));
        assert_eq!(graph.goal(), "CLI goal");
    }

    #[test]
    fn apply_goal_override_toml_wins_over_dot() {
        use crate::graph::types::{AttrValue, Graph};
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("original".to_string()),
        );
        apply_goal_override(&mut graph, None, Some("TOML goal"));
        assert_eq!(graph.goal(), "TOML goal");
    }

    #[test]
    fn apply_goal_override_noop_when_none() {
        use crate::graph::types::{AttrValue, Graph};
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("original".to_string()),
        );
        apply_goal_override(&mut graph, None, None);
        assert_eq!(graph.goal(), "original");
    }

    #[test]
    fn resolve_cli_goal_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("goal.md");
        std::fs::write(&path, "goal from file").unwrap();
        let result = resolve_cli_goal(&None, &Some(path)).unwrap();
        assert_eq!(result, Some("goal from file".to_string()));
    }

    #[test]
    fn resolve_cli_goal_from_string() {
        let result = resolve_cli_goal(&Some("inline goal".to_string()), &None).unwrap();
        assert_eq!(result, Some("inline goal".to_string()));
    }

    #[test]
    fn resolve_cli_goal_none() {
        let result = resolve_cli_goal(&None, &None).unwrap();
        assert_eq!(result, None);
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
            goal: Some("test".to_string()),
            graph: "test.fabro".to_string(),
            directory: None,
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
                fallbacks: None,
            }),
            setup: None,
            sandbox: None,
            vars: None,
            hooks: Vec::new(),
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
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
            goal: Some("test".to_string()),
            graph: "test.fabro".to_string(),
            directory: None,
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
                fallbacks: None,
            }),
            setup: None,
            sandbox: None,
            vars: None,
            hooks: Vec::new(),
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
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
                fallbacks: None,
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
                fallbacks: None,
            }),
            ..RunDefaults::default()
        };
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: Some("test".to_string()),
            graph: "test.fabro".to_string(),
            directory: None,
            llm: Some(run_config::LlmConfig {
                model: Some("toml-model".to_string()),
                provider: Some("openai".to_string()),
                fallbacks: None,
            }),
            setup: None,
            sandbox: None,
            vars: None,
            hooks: Vec::new(),
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
        };
        let (model, provider) = resolve_model_provider(None, None, Some(&cfg), &defaults, &graph);
        assert_eq!(model, "toml-model");
        assert_eq!(provider, Some("openai".to_string()));
    }

    #[test]
    fn resolve_preserve_sandbox_cli_wins() {
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: Some("test".to_string()),
            graph: "w.fabro".into(),
            directory: None,
            llm: None,
            setup: None,
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: Some(false),
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            vars: None,
            hooks: Vec::new(),
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
        };
        let defaults = RunDefaults::default();
        assert!(resolve_preserve_sandbox(true, Some(&cfg), &defaults));
    }

    #[test]
    fn resolve_preserve_sandbox_toml_wins_over_defaults() {
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: Some("test".to_string()),
            graph: "w.fabro".into(),
            directory: None,
            llm: None,
            setup: None,
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: Some(true),
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            vars: None,
            hooks: Vec::new(),
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
        };
        let defaults = RunDefaults {
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: Some(false),
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
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
                devcontainer: None,
                local: None,
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
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
    fn resolve_worktree_mode_defaults_to_clean() {
        let defaults = RunDefaults::default();
        assert_eq!(
            resolve_worktree_mode(None, &defaults),
            run_config::WorktreeMode::Clean
        );
    }

    #[test]
    fn resolve_worktree_mode_from_toml() {
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: Some("test".into()),
            graph: "w.fabro".into(),
            directory: None,
            llm: None,
            setup: None,
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: Some(run_config::LocalSandboxConfig {
                    worktree_mode: run_config::WorktreeMode::Always,
                }),
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            vars: None,
            hooks: Vec::new(),
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
        };
        let defaults = RunDefaults::default();
        assert_eq!(
            resolve_worktree_mode(Some(&cfg), &defaults),
            run_config::WorktreeMode::Always
        );
    }

    #[test]
    fn resolve_worktree_mode_from_defaults() {
        let defaults = RunDefaults {
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: Some(run_config::LocalSandboxConfig {
                    worktree_mode: run_config::WorktreeMode::Dirty,
                }),
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        assert_eq!(
            resolve_worktree_mode(None, &defaults),
            run_config::WorktreeMode::Dirty
        );
    }

    #[test]
    fn resolve_worktree_mode_toml_overrides_defaults() {
        let cfg = run_config::WorkflowRunConfig {
            version: 1,
            goal: Some("test".into()),
            graph: "w.fabro".into(),
            directory: None,
            llm: None,
            setup: None,
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: Some(run_config::LocalSandboxConfig {
                    worktree_mode: run_config::WorktreeMode::Never,
                }),
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            vars: None,
            hooks: Vec::new(),
            checkpoint: Default::default(),
            pull_request: None,
            assets: None,
            mcp_servers: Default::default(),
        };
        let defaults = RunDefaults {
            sandbox: Some(run_config::SandboxConfig {
                provider: None,
                preserve: None,
                devcontainer: None,
                local: Some(run_config::LocalSandboxConfig {
                    worktree_mode: run_config::WorktreeMode::Dirty,
                }),
                daytona: None,
                #[cfg(feature = "exedev")]
                exe: None,
                ssh: None,
                env: None,
            }),
            ..RunDefaults::default()
        };
        assert_eq!(
            resolve_worktree_mode(Some(&cfg), &defaults),
            run_config::WorktreeMode::Never
        );
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
        let redacted = fabro_util::redact::redact_jsonl_line(&compact);

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
        let redacted = fabro_util::redact::redact_jsonl_line(&pretty);

        assert!(!redacted.contains("AKIAYRWQG5EJLPZLBYNP"));
        assert!(redacted.contains("REDACTED"));

        let parsed: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(parsed["run_id"], "def-456");
    }

    #[test]
    fn envelope_field_order_starts_with_ts_run_id_event() {
        let event = crate::event::WorkflowRunEvent::StageStarted {
            node_id: "plan".to_string(),
            name: "Plan".to_string(),
            index: 0,
            handler_type: Some("agent".to_string()),
            script: None,
            attempt: 1,
            max_attempts: 3,
        };
        let envelope = build_event_envelope(&event, "run-123");
        let json = serde_json::to_string(&envelope).unwrap();
        // Parse the raw JSON to get field order
        let fields: Vec<String> = json
            .trim_start_matches('{')
            .trim_end_matches('}')
            .split(',')
            .filter_map(|pair| {
                let key = pair.split(':').next()?;
                Some(key.trim().trim_matches('"').to_string())
            })
            .collect();
        assert_eq!(&fields[0], "ts", "first field must be ts, got: {fields:?}");
        assert_eq!(
            &fields[1], "run_id",
            "second field must be run_id, got: {fields:?}"
        );
        assert_eq!(
            &fields[2], "event",
            "third field must be event, got: {fields:?}"
        );
    }
}
