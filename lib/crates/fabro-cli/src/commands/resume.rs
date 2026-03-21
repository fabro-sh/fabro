use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::Args;
use fabro_agent::{DockerSandbox, DockerSandboxConfig, Sandbox, WorktreeConfig, WorktreeSandbox};
use fabro_config::run::RunDefaults;
use fabro_graphviz::graph::Graph;
use fabro_interview::{AutoApproveInterviewer, ConsoleInterviewer, Interviewer};
use fabro_model::Provider;
use fabro_util::terminal::Styles;
use fabro_workflows::backend::{AgentApiBackend, AgentCliBackend, BackendRouter};
use fabro_workflows::checkpoint::Checkpoint;
use fabro_workflows::engine::RunConfig;
use fabro_workflows::event::EventEmitter;
use fabro_workflows::outcome::StageStatus;
use fabro_workflows::sandbox_provider::SandboxProvider;
use indicatif::HumanDuration;

use super::run::{
    apply_goal_override, build_event_envelope, default_run_dir, generate_retro,
    local_sandbox_with_callback, print_assets, print_final_output, resolve_cli_goal,
    resolve_daytona_config, resolve_model_provider, resolve_sandbox_provider,
    resolve_ssh_clone_params, resolve_ssh_config, write_finalize_commit, CliSandboxProvider,
};
use crate::commands::shared::{print_diagnostics, tilde_path};
use fabro_config::project as project_config;
use fabro_validate::Severity;

#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// Run ID, prefix, or branch (fabro/run/...)
    #[arg(required_unless_present = "checkpoint")]
    pub run: Option<String>,

    /// Resume from a checkpoint file (requires --workflow)
    #[arg(long, conflicts_with = "run", requires = "workflow")]
    pub checkpoint: Option<PathBuf>,

    /// Override workflow graph (required with --checkpoint)
    #[arg(long)]
    pub workflow: Option<PathBuf>,

    /// Run output directory
    #[arg(long)]
    pub run_dir: Option<PathBuf>,

    /// Execute with simulated LLM backend
    #[arg(long)]
    pub dry_run: bool,

    /// Auto-approve all human gates
    #[arg(long)]
    pub auto_approve: bool,

    /// Override the workflow goal (exposed as $goal in prompts)
    #[arg(long)]
    pub goal: Option<String>,

    /// Read the workflow goal from a file
    #[arg(long, conflicts_with = "goal")]
    pub goal_file: Option<PathBuf>,

    /// Override default LLM model
    #[arg(long)]
    pub model: Option<String>,

    /// Override default LLM provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Sandbox for agent tools
    #[arg(long, value_enum)]
    pub sandbox: Option<CliSandboxProvider>,

    /// Skip retro generation after the run
    #[arg(long)]
    pub no_retro: bool,

    /// Keep the sandbox alive after the run finishes (for debugging)
    #[arg(long)]
    pub preserve_sandbox: bool,
}

/// Intermediate state produced by the two resolution paths (checkpoint-file vs. git-branch).
struct ResumeContext {
    checkpoint: Checkpoint,
    graph: Graph,
    run_id: String,
    run_dir: PathBuf,
    sandbox: Arc<dyn Sandbox>,
    /// Kept as Arc so the sandbox event callbacks can emit through it. Listeners
    /// that need to be added later (e.g. ProgressUI) are registered separately.
    emitter: Arc<EventEmitter>,
    config: RunConfig,
    setup_commands: Vec<String>,
    /// Original cwd to restore after engine run (git-branch path changes cwd to worktree).
    original_cwd: Option<PathBuf>,
}

/// Resume an interrupted workflow run.
///
/// # Errors
///
/// Returns an error if the run cannot be found, the checkpoint cannot be loaded,
/// or the workflow cannot be resumed.
pub async fn resume_command(
    args: ResumeArgs,
    run_defaults: RunDefaults,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<()> {
    let ctx = if args.checkpoint.is_some() {
        prepare_from_checkpoint(&args, &run_defaults, styles, &github_app, git_author).await?
    } else {
        prepare_from_branch(&args, styles, &run_defaults, &github_app, git_author).await?
    };

    run_resumed(ctx, args, run_defaults, styles).await
}

/// Checkpoint-file path: load checkpoint and graph from files, resolve sandbox from flags/config.
async fn prepare_from_checkpoint(
    args: &ResumeArgs,
    run_defaults: &RunDefaults,
    styles: &Styles,
    github_app: &Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<ResumeContext> {
    let checkpoint_path = args.checkpoint.as_ref().unwrap();
    let workflow_path = args
        .workflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--workflow is required when using --checkpoint"))?;

    let checkpoint = Checkpoint::load(checkpoint_path)?;
    let (mut graph, diagnostics) = fabro_workflows::workflow::prepare_from_file(workflow_path)?;

    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    apply_goal_override(&mut graph, cli_goal.as_deref(), None);

    eprintln!(
        "{} {} from checkpoint {}",
        styles.bold.apply_to("Resuming workflow:"),
        graph.name,
        styles.dim.apply_to(checkpoint_path.display()),
    );

    print_diagnostics(&diagnostics, styles);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    let run_id = ulid::Ulid::new().to_string();
    let run_dir = args
        .run_dir
        .clone()
        .unwrap_or_else(|| default_run_dir(&run_id, args.dry_run));
    tokio::fs::create_dir_all(&run_dir).await?;
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;

    let original_cwd = std::env::current_dir()?;
    let emitter = Arc::new(EventEmitter::new());

    // Resolve sandbox provider from CLI flag / config / defaults
    let sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(args.sandbox.map(Into::into), None, run_defaults)?
    };

    let sandbox: Arc<dyn Sandbox> = match sandbox_provider {
        SandboxProvider::Local => {
            local_sandbox_with_callback(original_cwd.clone(), Arc::clone(&emitter))
        }
        SandboxProvider::Docker => {
            let config = DockerSandboxConfig {
                host_working_directory: original_cwd.to_string_lossy().to_string(),
                ..DockerSandboxConfig::default()
            };
            let mut env = DockerSandbox::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker environment: {e}"))?;
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => {
            let exe_config = super::run::resolve_exe_config(None, run_defaults);
            let clone_params = super::run::resolve_exe_clone_params(&original_cwd);
            let mgmt_ssh = fabro_sandbox::exe::OpensshRunner::connect_raw("exe.dev")
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to exe.dev: {e}"))?;
            let config = exe_config.unwrap_or_default();
            let mut env = fabro_sandbox::exe::ExeSandbox::new(
                Box::new(mgmt_ssh),
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Ssh => {
            let config = resolve_ssh_config(None, run_defaults)
                .ok_or_else(|| anyhow::anyhow!("--sandbox ssh requires [sandbox.ssh] config"))?;
            let clone_params = resolve_ssh_clone_params(&original_cwd);
            let mut env = fabro_sandbox::ssh::SshSandbox::new(
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
        SandboxProvider::Daytona => {
            let config = resolve_daytona_config(None, run_defaults).unwrap_or_default();
            let mut env = fabro_sandbox::daytona::DaytonaSandbox::new(
                config,
                github_app.clone(),
                Some(run_id.clone()),
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(env)
        }
    };
    let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::ReadBeforeWriteSandbox::new(sandbox));

    let config = RunConfig {
        run_dir: run_dir.clone(),
        cancel_token: None,
        dry_run: args.dry_run,
        run_id: run_id.clone(),
        git_checkpoint_enabled: false,
        host_repo_path: None,
        base_sha: None,
        run_branch: None,
        meta_branch: None,
        labels: HashMap::new(),
        checkpoint_exclude_globs: Vec::new(),
        github_app: github_app.clone(),
        git_author,
        base_branch: None,
        pull_request: None,
        asset_globs: Vec::new(),
        workflow_slug: None,
    };

    Ok(ResumeContext {
        checkpoint,
        graph,
        run_id,
        run_dir,
        sandbox,
        emitter,
        config,
        setup_commands: Vec::new(),
        original_cwd: None,
    })
}

/// Git-branch path: resolve run ID, read checkpoint + graph from metadata, set up worktree.
async fn prepare_from_branch(
    args: &ResumeArgs,
    styles: &Styles,
    run_defaults: &RunDefaults,
    github_app: &Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<ResumeContext> {
    let run_arg = args.run.as_deref().expect("run is required");

    let (run_id, run_branch) =
        if let Some(stripped) = run_arg.strip_prefix(fabro_workflows::git::RUN_BRANCH_PREFIX) {
            (stripped.to_string(), run_arg.to_string())
        } else {
            let repo = git2::Repository::discover(".").context("not in a git repository")?;
            let id = fabro_workflows::run_rewind::find_run_id_by_prefix(&repo, run_arg)?;
            let branch = format!("{}{}", fabro_workflows::git::RUN_BRANCH_PREFIX, id);
            (id, branch)
        };

    let original_cwd = std::env::current_dir()?;

    // Read checkpoint from metadata branch
    let checkpoint = fabro_workflows::git::MetadataStore::read_checkpoint(&original_cwd, &run_id)?
        .ok_or_else(|| {
            anyhow::anyhow!("no checkpoint found on metadata branch for run {run_id}")
        })?;

    // Read graph DOT from metadata branch
    let source = fabro_workflows::git::MetadataStore::read_graph_dot(&original_cwd, &run_id)?
        .ok_or_else(|| {
            anyhow::anyhow!("no graph.fabro found on metadata branch for run {run_id}")
        })?;

    // If --workflow was also provided, use it instead (allows overriding)
    let (mut graph, diagnostics) = if let Some(ref workflow_path) = args.workflow {
        fabro_workflows::workflow::prepare_from_file(workflow_path)?
    } else {
        fabro_workflows::workflow::WorkflowBuilder::new().prepare(&source)?
    };
    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    apply_goal_override(&mut graph, cli_goal.as_deref(), None);

    eprintln!(
        "{} {} from branch {} ({})",
        styles.bold.apply_to("Resuming workflow:"),
        graph.name,
        styles.dim.apply_to(&run_branch),
        run_id,
    );

    print_diagnostics(&diagnostics, styles);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    // Set up logs directory — reuse existing run dir for this run_id to avoid
    // "ambiguous prefix" errors when the resume happens on a different day.
    let run_dir = if let Some(ref dir) = args.run_dir {
        dir.clone()
    } else {
        find_existing_run_dir(&run_id).unwrap_or_else(|| default_run_dir(&run_id, args.dry_run))
    };
    tokio::fs::create_dir_all(&run_dir).await?;
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;
    tokio::fs::write(run_dir.join("graph.fabro"), &source).await?;

    let base_sha = fabro_workflows::git::MetadataStore::read_manifest(&original_cwd, &run_id)?
        .and_then(|m| m.base_sha);

    // Resolve sandbox provider
    let sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(args.sandbox.map(Into::into), None, run_defaults)?
    };

    let emitter = Arc::new(EventEmitter::new());
    let (sandbox, _worktree_path): (Arc<dyn Sandbox>, Option<PathBuf>) = match sandbox_provider {
        SandboxProvider::Local | SandboxProvider::Docker => {
            // Re-attach worktree to the existing run branch via WorktreeSandbox.
            let wt = run_dir.join("worktree");
            let wt_str = wt.to_string_lossy().into_owned();

            let inner = local_sandbox_with_callback(original_cwd.clone(), Arc::clone(&emitter));
            let wt_config = WorktreeConfig {
                branch_name: run_branch.clone(),
                base_sha: base_sha.clone().unwrap_or_default(),
                worktree_path: wt_str.clone(),
                skip_branch_creation: true, // branch already exists on resume
            };
            let mut wt_sandbox = WorktreeSandbox::new(inner, wt_config);
            wt_sandbox.set_event_callback(Arc::clone(&emitter).worktree_callback());

            wt_sandbox
                .initialize()
                .await
                .map_err(|e| anyhow::anyhow!("failed to attach worktree to {run_branch}: {e}"))?;
            std::env::set_current_dir(&wt)?;
            (Arc::new(wt_sandbox) as Arc<dyn Sandbox>, Some(wt))
        }
        #[cfg(feature = "exedev")]
        SandboxProvider::Exe => {
            let exe_config = super::run::resolve_exe_config(None, run_defaults);
            let clone_params = super::run::resolve_exe_clone_params(&original_cwd);
            let mgmt_ssh = fabro_sandbox::exe::OpensshRunner::connect_raw("exe.dev")
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to exe.dev: {e}"))?;
            let config = exe_config.unwrap_or_default();
            let mut env = fabro_sandbox::exe::ExeSandbox::new(
                Box::new(mgmt_ssh),
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            (Arc::new(env), None)
        }
        SandboxProvider::Ssh => {
            let config = resolve_ssh_config(None, run_defaults)
                .ok_or_else(|| anyhow::anyhow!("--sandbox ssh requires [sandbox.ssh] config"))?;
            let clone_params = resolve_ssh_clone_params(&original_cwd);
            let mut env = fabro_sandbox::ssh::SshSandbox::new(
                config,
                clone_params,
                Some(run_id.clone()),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            (Arc::new(env), None)
        }
        SandboxProvider::Daytona => {
            let config = resolve_daytona_config(None, run_defaults).unwrap_or_default();
            let mut env = fabro_sandbox::daytona::DaytonaSandbox::new(
                config,
                github_app.clone(),
                Some(run_id.clone()),
                Some(run_branch.clone()),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            let emitter_cb = Arc::clone(&emitter);
            env.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&fabro_workflows::event::WorkflowRunEvent::Sandbox { event });
            }));
            (Arc::new(env), None)
        }
    };

    // Wrap with ReadBeforeWriteSandbox to enforce read-before-write guard
    let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::ReadBeforeWriteSandbox::new(sandbox));

    // Let the sandbox provide any commands needed to resume on the existing run branch
    let setup_commands: Vec<String> = sandbox.resume_setup_commands(&run_branch);

    let meta_branch = Some(fabro_workflows::git::MetadataStore::branch_name(&run_id));
    let config = RunConfig {
        run_dir: run_dir.clone(),
        cancel_token: None,
        dry_run: args.dry_run,
        run_id: run_id.clone(),
        git_checkpoint_enabled: true,
        host_repo_path: Some(original_cwd.clone()),
        base_sha,
        run_branch: Some(run_branch),
        meta_branch,
        labels: HashMap::new(),
        checkpoint_exclude_globs: Vec::new(),
        github_app: github_app.clone(),
        git_author,
        base_branch: None,
        pull_request: None,
        asset_globs: Vec::new(),
        workflow_slug: None,
    };

    Ok(ResumeContext {
        checkpoint,
        graph,
        run_id,
        run_dir,
        sandbox,
        emitter,
        config,
        setup_commands,
        original_cwd: Some(original_cwd),
    })
}

/// Shared tail: build engine, run workflow, generate retro, print results.
async fn run_resumed(
    ctx: ResumeContext,
    args: ResumeArgs,
    run_defaults: RunDefaults,
    styles: &'static Styles,
) -> anyhow::Result<()> {
    let ResumeContext {
        checkpoint,
        graph,
        run_id,
        run_dir,
        sandbox,
        emitter,
        mut config,
        setup_commands,
        original_cwd,
    } = ctx;

    // Write run.pid + initial status so `fabro ps` / `fabro attach` can find this run
    tokio::fs::write(run_dir.join("run.pid"), std::process::id().to_string()).await?;
    fabro_workflows::run_status::write_run_status(
        &run_dir,
        fabro_workflows::run_status::RunStatus::Starting,
        Some(fabro_workflows::run_status::StatusReason::SandboxInitializing),
    );

    // Safety net: mark as failed if we exit before engine.run() completes
    let status_run_dir = run_dir.clone();
    let status_guard = scopeguard::guard((), move |()| {
        fabro_workflows::run_status::write_run_status(
            &status_run_dir,
            fabro_workflows::run_status::RunStatus::Failed,
            Some(fabro_workflows::run_status::StatusReason::SandboxInitFailed),
        );
    });

    // Track the last git commit SHA from CheckpointCompleted events
    let last_git_sha: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    {
        let sha_clone = Arc::clone(&last_git_sha);
        emitter.on_event(move |event| {
            if let fabro_workflows::event::WorkflowRunEvent::CheckpointCompleted {
                git_commit_sha: Some(sha),
                ..
            } = event
            {
                *sha_clone.lock().unwrap() = Some(sha.clone());
            }
        });
    }

    // Create progress UI (verbose mode shows detailed turn/tool counts and token usage)
    let is_tty = std::io::stderr().is_terminal();
    let progress_ui = Arc::new(std::sync::Mutex::new(super::run_progress::ProgressUI::new(
        is_tty,
        args.verbose,
    )));
    {
        let p = Arc::clone(&progress_ui);
        emitter.on_event(move |event| {
            let mut ui = p.lock().expect("progress lock poisoned");
            ui.handle_event(event);
        });
    }

    // JSONL progress log + live.json snapshot (mirrors run_command)
    {
        let jsonl_path = run_dir.join("progress.jsonl");
        let live_path = run_dir.join("live.json");
        let run_id_shared = Arc::new(std::sync::Mutex::new(run_id.clone()));
        let run_id_clone = Arc::clone(&run_id_shared);
        emitter.on_event(move |event| {
            if let fabro_workflows::event::WorkflowRunEvent::WorkflowRunStarted { run_id, .. } =
                event
            {
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

    let interviewer: Arc<dyn Interviewer> = if args.auto_approve {
        Arc::new(AutoApproveInterviewer)
    } else {
        Arc::new(ConsoleInterviewer::new(styles))
    };

    let dry_run_mode = args.dry_run
        || fabro_llm::client::Client::from_env()
            .await
            .map(|c| c.provider_names().is_empty())
            .unwrap_or(true);
    config.dry_run = dry_run_mode;

    let (model, provider) = resolve_model_provider(
        args.model.as_deref(),
        args.provider.as_deref(),
        None,
        &run_defaults,
        &graph,
    );
    let provider_enum: Provider = provider
        .as_deref()
        .map(|s| s.parse::<Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or_else(Provider::default_from_env);

    let fallback_chain = match run_defaults.llm.as_ref().and_then(|l| l.fallbacks.as_ref()) {
        Some(map) => fabro_model::build_fallback_chain(provider_enum.as_str(), &model, map),
        None => Vec::new(),
    };

    let registry = fabro_workflows::handler::default_registry(interviewer.clone(), || {
        if dry_run_mode {
            None
        } else {
            let api = AgentApiBackend::new(model.clone(), provider_enum, fallback_chain.clone());
            let cli = AgentCliBackend::new(model.clone(), provider_enum);
            Some(Box::new(BackendRouter::new(Box::new(api), cli)))
        }
    });
    let mut engine = fabro_workflows::engine::WorkflowRunEngine::with_interviewer(
        registry,
        Arc::clone(&emitter),
        interviewer,
        Arc::clone(&sandbox),
    );
    if dry_run_mode {
        engine.set_dry_run(true);
    }

    let lifecycle = fabro_workflows::engine::LifecycleConfig {
        setup_commands,
        setup_command_timeout_ms: 60_000,
        devcontainer_phases: Vec::new(),
    };

    let run_start = Instant::now();
    let engine_result = engine
        .run_with_lifecycle(&graph, &mut config, lifecycle, Some(&checkpoint))
        .await;
    let run_duration_ms = run_start.elapsed().as_millis() as u64;

    // Defuse the status guard — we made it past engine.run()
    scopeguard::ScopeGuard::into_inner(status_guard);

    // Finish progress bars before retro
    progress_ui.lock().expect("progress lock poisoned").finish();

    // Restore cwd if we changed it (worktree is kept for `fabro cp` access; pruned separately)
    if let Some(ref cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }

    // Build and save conclusion.json + final status (mirrors run_command)
    {
        let (status, failure_reason) = match &engine_result {
            Ok(ref o) => (o.status.clone(), o.failure_reason().map(String::from)),
            Err(e) => (StageStatus::Fail, Some(e.to_string())),
        };

        let (run_status, status_reason) = match &engine_result {
            Ok(ref o) => match o.status {
                StageStatus::Success | StageStatus::Skipped => (
                    fabro_workflows::run_status::RunStatus::Succeeded,
                    Some(fabro_workflows::run_status::StatusReason::Completed),
                ),
                StageStatus::PartialSuccess => (
                    fabro_workflows::run_status::RunStatus::Succeeded,
                    Some(fabro_workflows::run_status::StatusReason::PartialSuccess),
                ),
                StageStatus::Fail | StageStatus::Retry => (
                    fabro_workflows::run_status::RunStatus::Failed,
                    Some(fabro_workflows::run_status::StatusReason::WorkflowError),
                ),
            },
            Err(fabro_workflows::error::FabroError::Cancelled) => (
                fabro_workflows::run_status::RunStatus::Failed,
                Some(fabro_workflows::run_status::StatusReason::Cancelled),
            ),
            Err(_) => (
                fabro_workflows::run_status::RunStatus::Failed,
                Some(fabro_workflows::run_status::StatusReason::WorkflowError),
            ),
        };

        let checkpoint_loaded = Checkpoint::load(&run_dir.join("checkpoint.json")).ok();
        let stage_durations = fabro_retro::retro::extract_stage_durations(&run_dir);

        let (stages, total_cost, total_retries) = if let Some(ref cp) = checkpoint_loaded {
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

                stages.push(fabro_workflows::conclusion::StageSummary {
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

        let conclusion = fabro_workflows::conclusion::Conclusion {
            timestamp: chrono::Utc::now(),
            status,
            duration_ms: run_duration_ms,
            failure_reason,
            final_git_commit_sha: last_git_sha.lock().unwrap().clone(),
            stages,
            total_cost,
            total_retries,
        };
        let _ = conclusion.save(&run_dir.join("conclusion.json"));
        fabro_workflows::run_status::write_run_status(&run_dir, run_status, status_reason);
    }

    // Auto-derive retro
    if !args.no_retro && project_config::is_retro_enabled() {
        let failed = match &engine_result {
            Ok(ref o) => o.status == StageStatus::Fail,
            Err(_) => true,
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
            run_duration_ms,
            dry_run_mode,
            llm_client.as_ref(),
            &sandbox,
            provider_enum,
            &model,
            styles,
            Some(Arc::clone(&emitter)),
        )
        .await;
    }

    // Write finalize commit with retro.json + final node files (captures last diff.patch)
    write_finalize_commit(&config, &run_dir).await;

    // Cleanup sandbox via engine (fires SandboxCleanup hook)
    use super::run::resolve_preserve_sandbox;
    let preserve = resolve_preserve_sandbox(args.preserve_sandbox, None, &run_defaults);
    let _ = engine
        .cleanup_sandbox(&config.run_id, &graph.name, preserve)
        .await;

    let outcome = engine_result?;

    eprintln!("\n{}", styles.bold.apply_to("=== Run Result ==="));
    eprintln!("{}", styles.dim.apply_to(format!("Run:       {run_id}")));
    let status_str = outcome.status.to_string().to_uppercase();
    let status_color = match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess => &styles.bold_green,
        _ => &styles.bold_red,
    };
    eprintln!("Status:    {}", status_color.apply_to(&status_str));
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

/// Scan `~/.fabro/runs/` for an existing directory whose name ends with `-{run_id}`.
fn find_existing_run_dir(run_id: &str) -> Option<PathBuf> {
    let base = dirs::home_dir()?.join(".fabro").join("runs");
    let suffix = format!("-{run_id}");
    let entries = std::fs::read_dir(&base).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(&suffix) && entry.path().is_dir() {
            return Some(entry.path());
        }
    }
    None
}
