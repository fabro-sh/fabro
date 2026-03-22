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
    local_sandbox_with_callback, mint_github_token, print_assets, print_final_output,
    resolve_cli_goal, resolve_daytona_config, resolve_model_provider, resolve_sandbox_provider,
    resolve_ssh_clone_params, resolve_ssh_config, write_finalize_commit, CliSandboxProvider,
};
use crate::commands::shared::{print_diagnostics, read_workflow_file, tilde_path};
use fabro_config::project as project_config;
use fabro_config::run as run_config;
use fabro_validate::Severity;
use fabro_workflows::devcontainer_bridge;
use std::collections::HashMap;
use tracing::debug;

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

    /// Attach a label to this run (repeatable, format: KEY=VALUE)
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,
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
    /// Devcontainer lifecycle phases (on_create, post_create, post_start) resolved from config.
    devcontainer_phases: Vec<(String, Vec<fabro_devcontainer::Command>)>,
    /// Devcontainer remoteEnv values to layer under sandbox_env.
    devcontainer_env: HashMap<String, String>,
    /// Original cwd to restore after engine run (git-branch path changes cwd to worktree).
    original_cwd: Option<PathBuf>,
    sandbox_provider: SandboxProvider,
    ssh_data_host: Option<String>,
    github_app: Option<fabro_github::GitHubAppCredentials>,
}

/// Resume an interrupted workflow run.
///
/// # Errors
///
/// Returns an error if the run cannot be found, the checkpoint cannot be loaded,
/// or the workflow cannot be resumed.
pub async fn resume_command(
    args: ResumeArgs,
    mut run_defaults: RunDefaults,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<()> {
    // Apply project-level config overrides (fabro.toml) on top of CLI defaults (mirrors run_command).
    if let Ok(Some((_config_path, project_config))) =
        project_config::discover_project_config(&std::env::current_dir().unwrap_or_default())
    {
        tracing::debug!("Applying run defaults from fabro.toml");
        run_defaults.merge_overlay(project_config.into_run_defaults());
    }

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

    // Expand {{vars.xxx}} placeholders before parsing (mirrors run_command)
    let source = read_workflow_file(workflow_path)?;
    let vars = run_defaults.vars.as_ref();
    let source = match vars {
        Some(vars) => fabro_workflows::vars::expand_vars(&source, vars)?,
        None => source,
    };
    let dot_dir = workflow_path.parent().unwrap_or(std::path::Path::new("."));
    let (mut graph, diagnostics) = fabro_workflows::workflow::WorkflowBuilder::new()
        .prepare_with_file_inlining(&source, dot_dir)?;

    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    apply_goal_override(&mut graph, cli_goal.as_deref(), None);

    // Inline @file references in the (possibly overridden) goal (mirrors run_command)
    if let Some(fabro_graphviz::graph::AttrValue::String(goal)) = graph.attrs.get("goal") {
        let fallback = dirs::home_dir().map(|h| h.join(".fabro"));
        let resolved =
            fabro_workflows::transform::resolve_file_ref(goal, dot_dir, fallback.as_deref());
        if resolved != *goal {
            graph.attrs.insert(
                "goal".to_string(),
                fabro_graphviz::graph::AttrValue::String(resolved),
            );
        }
    }

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
    tokio::fs::write(
        run_dir.join("graph.fabro"),
        tokio::fs::read(workflow_path).await?,
    )
    .await?;

    let original_cwd = std::env::current_dir()?;
    let emitter = Arc::new(EventEmitter::new());

    // Resolve sandbox provider from CLI flag / config / defaults
    let sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(args.sandbox.map(Into::into), None, run_defaults)?
    };

    // Resolve devcontainer BEFORE sandbox creation (mirrors run_command) so that
    // the Daytona snapshot config can be overridden with the devcontainer Dockerfile.
    let mut daytona_config = resolve_daytona_config(None, run_defaults);
    let devcontainer_config = if run_defaults
        .sandbox
        .as_ref()
        .and_then(|s| s.devcontainer)
        .unwrap_or(false)
    {
        match fabro_devcontainer::DevcontainerResolver::resolve(&original_cwd).await {
            Ok(dc) => {
                let lifecycle_command_count = dc.on_create_commands.len()
                    + dc.post_create_commands.len()
                    + dc.post_start_commands.len();
                emitter.emit(
                    &fabro_workflows::event::WorkflowRunEvent::DevcontainerResolved {
                        dockerfile_lines: dc.dockerfile.lines().count(),
                        environment_count: dc.environment.len(),
                        lifecycle_command_count,
                        workspace_folder: dc.workspace_folder.clone(),
                    },
                );

                // Override daytona_config with devcontainer dockerfile
                let snapshot = devcontainer_bridge::devcontainer_to_snapshot_config(&dc);
                let mut cfg = daytona_config.unwrap_or_default();
                cfg.snapshot = Some(snapshot);
                daytona_config = Some(cfg);

                // Run initialize_commands on host (mirrors run_command)
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
                            .current_dir(&original_cwd)
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

    let devcontainer_phases = if let Some(ref dc) = devcontainer_config {
        vec![
            ("on_create".to_string(), dc.on_create_commands.clone()),
            ("post_create".to_string(), dc.post_create_commands.clone()),
            ("post_start".to_string(), dc.post_start_commands.clone()),
        ]
    } else {
        Vec::new()
    };

    let mut ssh_data_host: Option<String> = None;
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
            ssh_data_host = Some(config.destination.clone());
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
            let config = daytona_config.unwrap_or_default();
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
        labels: args
            .label
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        checkpoint_exclude_globs: run_defaults.checkpoint.exclude_globs.clone(),
        github_app: github_app.clone(),
        git_author,
        base_branch: None,
        pull_request: run_defaults
            .pull_request
            .as_ref()
            .filter(|p| p.enabled)
            .cloned(),
        asset_globs: run_defaults
            .assets
            .as_ref()
            .map(|a| a.include.clone())
            .unwrap_or_default(),
        workflow_slug: None,
    };

    let devcontainer_env = devcontainer_config
        .as_ref()
        .map(|dc| dc.environment.clone())
        .unwrap_or_default();

    Ok(ResumeContext {
        checkpoint,
        graph,
        run_id,
        run_dir,
        sandbox,
        emitter,
        config,
        setup_commands: run_defaults
            .setup
            .as_ref()
            .map(|s| s.commands.clone())
            .unwrap_or_default(),
        devcontainer_phases,
        devcontainer_env,
        original_cwd: None,
        sandbox_provider,
        ssh_data_host,
        github_app: github_app.clone(),
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
    let detected_base_branch = fabro_sandbox::daytona::detect_repo_info(&original_cwd)
        .map(|(_, branch)| branch)
        .unwrap_or(None);

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
        // Expand {{vars.xxx}} placeholders before parsing (mirrors run_command)
        let wf_source = read_workflow_file(workflow_path)?;
        let vars = run_defaults.vars.as_ref();
        let wf_source = match vars {
            Some(vars) => fabro_workflows::vars::expand_vars(&wf_source, vars)?,
            None => wf_source,
        };
        let dot_dir = workflow_path.parent().unwrap_or(std::path::Path::new("."));
        fabro_workflows::workflow::WorkflowBuilder::new()
            .prepare_with_file_inlining(&wf_source, dot_dir)?
    } else {
        fabro_workflows::workflow::WorkflowBuilder::new().prepare(&source)?
    };
    let cli_goal = resolve_cli_goal(&args.goal, &args.goal_file)?;
    apply_goal_override(&mut graph, cli_goal.as_deref(), None);

    // Inline @file references in the (possibly overridden) goal (mirrors run_command)
    {
        let dot_dir = args
            .workflow
            .as_deref()
            .and_then(|p| p.parent())
            .unwrap_or(std::path::Path::new("."));
        if let Some(fabro_graphviz::graph::AttrValue::String(goal)) = graph.attrs.get("goal") {
            let fallback = dirs::home_dir().map(|h| h.join(".fabro"));
            let resolved =
                fabro_workflows::transform::resolve_file_ref(goal, dot_dir, fallback.as_deref());
            if resolved != *goal {
                graph.attrs.insert(
                    "goal".to_string(),
                    fabro_graphviz::graph::AttrValue::String(resolved),
                );
            }
        }
    }

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
    // Skip reuse when dry-running to avoid corrupting real run data.
    let run_dir = if let Some(ref dir) = args.run_dir {
        dir.clone()
    } else if args.dry_run {
        default_run_dir(&run_id, true)
    } else {
        find_existing_run_dir(&run_id).unwrap_or_else(|| default_run_dir(&run_id, false))
    };
    tokio::fs::create_dir_all(&run_dir).await?;
    let run_dir = tokio::fs::canonicalize(&run_dir).await.unwrap_or(run_dir);
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;
    if let Some(ref workflow_path) = args.workflow {
        tokio::fs::write(
            run_dir.join("graph.fabro"),
            tokio::fs::read(workflow_path).await?,
        )
        .await?;
    } else {
        tokio::fs::write(run_dir.join("graph.fabro"), &source).await?;
    }

    let base_sha = fabro_workflows::git::MetadataStore::read_manifest(&original_cwd, &run_id)?
        .and_then(|m| m.base_sha);

    // Resolve sandbox provider
    let mut sandbox_provider = if args.dry_run {
        SandboxProvider::Local
    } else {
        resolve_sandbox_provider(args.sandbox.map(Into::into), None, run_defaults)?
    };

    let emitter = Arc::new(EventEmitter::new());

    // Resolve devcontainer BEFORE sandbox creation (mirrors run_command) so that
    // the Daytona snapshot config can be overridden with the devcontainer Dockerfile.
    let mut daytona_config = resolve_daytona_config(None, run_defaults);
    let devcontainer_config = if run_defaults
        .sandbox
        .as_ref()
        .and_then(|s| s.devcontainer)
        .unwrap_or(false)
    {
        match fabro_devcontainer::DevcontainerResolver::resolve(&original_cwd).await {
            Ok(dc) => {
                let lifecycle_command_count = dc.on_create_commands.len()
                    + dc.post_create_commands.len()
                    + dc.post_start_commands.len();
                emitter.emit(
                    &fabro_workflows::event::WorkflowRunEvent::DevcontainerResolved {
                        dockerfile_lines: dc.dockerfile.lines().count(),
                        environment_count: dc.environment.len(),
                        lifecycle_command_count,
                        workspace_folder: dc.workspace_folder.clone(),
                    },
                );

                // Override daytona_config with devcontainer dockerfile
                let snapshot = devcontainer_bridge::devcontainer_to_snapshot_config(&dc);
                let mut cfg = daytona_config.unwrap_or_default();
                cfg.snapshot = Some(snapshot);
                daytona_config = Some(cfg);

                // Run initialize_commands on host (mirrors run_command)
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
                            .current_dir(&original_cwd)
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

    let devcontainer_phases = if let Some(ref dc) = devcontainer_config {
        vec![
            ("on_create".to_string(), dc.on_create_commands.clone()),
            ("post_create".to_string(), dc.post_create_commands.clone()),
            ("post_start".to_string(), dc.post_start_commands.clone()),
        ]
    } else {
        Vec::new()
    };

    let setup_worktree_sandbox = |emitter: &Arc<EventEmitter>| -> (WorktreeSandbox, PathBuf) {
        let wt = run_dir.join("worktree");
        let wt_str = wt.to_string_lossy().into_owned();

        let inner = local_sandbox_with_callback(original_cwd.clone(), Arc::clone(emitter));
        let wt_config = WorktreeConfig {
            branch_name: run_branch.clone(),
            base_sha: base_sha.clone().unwrap_or_default(),
            worktree_path: wt_str.clone(),
            skip_branch_creation: true, // branch already exists on resume
        };
        let mut wt_sandbox = WorktreeSandbox::new(inner, wt_config);
        wt_sandbox.set_event_callback(Arc::clone(emitter).worktree_callback());
        (wt_sandbox, wt)
    };

    let mut ssh_data_host: Option<String> = None;
    let (sandbox, _worktree_path): (Arc<dyn Sandbox>, Option<PathBuf>) = match sandbox_provider {
        SandboxProvider::Local => {
            let (wt_sandbox, wt) = setup_worktree_sandbox(&emitter);
            wt_sandbox
                .initialize()
                .await
                .map_err(|e| anyhow::anyhow!("failed to attach worktree to {run_branch}: {e}"))?;
            std::env::set_current_dir(&wt)?;
            (Arc::new(wt_sandbox) as Arc<dyn Sandbox>, Some(wt))
        }
        SandboxProvider::Docker => {
            tracing::warn!(
                "--sandbox docker is not supported for branch resume; falling back to local worktree sandbox"
            );
            eprintln!(
                "{} --sandbox docker is not supported for branch resume; falling back to local worktree sandbox.",
                styles.yellow.apply_to("Warning:"),
            );
            sandbox_provider = SandboxProvider::Local;
            let (wt_sandbox, wt) = setup_worktree_sandbox(&emitter);
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
            ssh_data_host = Some(config.destination.clone());
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
            let config = daytona_config.unwrap_or_default();
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

    // User-configured setup commands first, then sandbox-specific resume commands
    let mut setup_commands: Vec<String> = run_defaults
        .setup
        .as_ref()
        .map(|s| s.commands.clone())
        .unwrap_or_default();
    setup_commands.extend(sandbox.resume_setup_commands(&run_branch));

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
        labels: args
            .label
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        checkpoint_exclude_globs: run_defaults.checkpoint.exclude_globs.clone(),
        github_app: github_app.clone(),
        git_author,
        base_branch: detected_base_branch,
        pull_request: run_defaults
            .pull_request
            .as_ref()
            .filter(|p| p.enabled)
            .cloned(),
        asset_globs: run_defaults
            .assets
            .as_ref()
            .map(|a| a.include.clone())
            .unwrap_or_default(),
        workflow_slug: None,
    };

    let devcontainer_env = devcontainer_config
        .as_ref()
        .map(|dc| dc.environment.clone())
        .unwrap_or_default();

    Ok(ResumeContext {
        checkpoint,
        graph,
        run_id,
        run_dir,
        sandbox,
        emitter,
        config,
        setup_commands,
        devcontainer_phases,
        devcontainer_env,
        original_cwd: Some(original_cwd),
        sandbox_provider,
        ssh_data_host,
        github_app: github_app.clone(),
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
        devcontainer_phases,
        devcontainer_env,
        original_cwd,
        sandbox_provider,
        ssh_data_host,
        github_app,
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

    // Cost accumulator (mirrors run_command)
    let accumulator = Arc::new(std::sync::Mutex::new(super::run::CostAccumulator::default()));
    {
        let acc_clone = Arc::clone(&accumulator);
        emitter.on_event(move |event| {
            if let fabro_workflows::event::WorkflowRunEvent::StageCompleted {
                usage: Some(u), ..
            } = event
            {
                let mut acc = acc_clone.lock().unwrap();
                acc.total_input_tokens += u.input_tokens;
                acc.total_output_tokens += u.output_tokens;
                acc.total_cache_read_tokens += u.cache_read_tokens.unwrap_or(0);
                acc.total_cache_write_tokens += u.cache_write_tokens.unwrap_or(0);
                acc.total_reasoning_tokens += u.reasoning_tokens.unwrap_or(0);
                if let Some(cost) = fabro_workflows::cost::compute_stage_cost(u) {
                    acc.total_cost += cost;
                    acc.has_pricing = true;
                }
            }
        });
    }

    // Write sandbox.json when sandbox is initialized (mirrors run_command)
    {
        let run_dir_for_listener = run_dir.clone();
        let progress_for_listener = Arc::clone(&progress_ui);
        let cwd_for_listener = match &original_cwd {
            Some(p) => p.to_string_lossy().to_string(),
            // original_cwd is None only for checkpoint path where cwd hasn't changed
            None => std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .to_string(),
        };
        let sandbox_for_listener = Arc::clone(&sandbox);
        let provider = sandbox_provider;
        let ssh_host = ssh_data_host.clone();
        emitter.on_event(move |event| {
            if let fabro_workflows::event::WorkflowRunEvent::SandboxInitialized {
                working_directory,
            } = event
            {
                progress_for_listener
                    .lock()
                    .expect("progress lock poisoned")
                    .set_working_directory(working_directory.clone());

                let sandbox_info_opt = {
                    let info = sandbox_for_listener.sandbox_info();
                    if info.is_empty() {
                        None
                    } else {
                        Some(info)
                    }
                };

                let is_docker = provider == SandboxProvider::Docker;
                let record = fabro_workflows::sandbox_record::SandboxRecord {
                    provider: provider.to_string(),
                    working_directory: working_directory.clone(),
                    identifier: sandbox_info_opt,
                    host_working_directory: if is_docker {
                        Some(cwd_for_listener.clone())
                    } else {
                        None
                    },
                    container_mount_point: if is_docker {
                        Some(working_directory.clone())
                    } else {
                        None
                    },
                    data_host: if provider == SandboxProvider::Ssh {
                        ssh_host.clone()
                    } else {
                        None
                    },
                };
                if let Err(e) = record.save(&run_dir_for_listener.join("sandbox.json")) {
                    tracing::warn!(error = %e, "Failed to save sandbox record");
                }
            }
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
        Arc::new(super::run_progress::ProgressAwareInterviewer::new(
            ConsoleInterviewer::new(styles),
            Arc::clone(&progress_ui),
        ))
    };

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

    // Build sandbox env: devcontainer env layered underneath TOML env (TOML wins on conflict, mirrors run_command)
    let sandbox_env: HashMap<String, String> = {
        let mut env = devcontainer_env;
        if let Some(mut toml_env) = run_defaults.sandbox.as_ref().and_then(|s| s.env.clone()) {
            run_config::resolve_env_refs(&mut toml_env)?;
            env.extend(toml_env);
        }
        env
    };

    // Mint a GitHub App IAT and inject as GITHUB_TOKEN if [github] permissions are declared
    let mut sandbox_env = sandbox_env;
    let github_permissions = run_defaults.github.as_ref();
    if let Some(gh_cfg) = github_permissions {
        if !gh_cfg.permissions.is_empty() {
            let origin_url = fabro_sandbox::daytona::detect_repo_info(
                original_cwd
                    .as_deref()
                    .unwrap_or_else(|| std::path::Path::new(".")),
            )
            .ok()
            .map(|(url, _)| url);
            if let (Some(ref creds), Some(ref url)) = (&github_app, &origin_url) {
                match mint_github_token(creds, url, &gh_cfg.permissions).await {
                    Ok(token) => {
                        debug!("Minted GitHub IAT for sandbox GITHUB_TOKEN");
                        sandbox_env.insert("GITHUB_TOKEN".to_string(), token);
                    }
                    Err(e) => {
                        eprintln!(
                            "{} Failed to mint GitHub token: {e}",
                            styles.yellow.apply_to("Warning:"),
                        );
                    }
                }
            } else {
                debug!("Skipping GitHub token: no GitHub App credentials or origin URL");
            }
        }
    }

    // Resolve MCP servers from run defaults
    let mcp_servers: Vec<fabro_mcp::config::McpServerConfig> = run_defaults
        .mcp_servers
        .clone()
        .into_iter()
        .map(|(name, entry)| entry.into_config(name))
        .collect();

    let registry = fabro_workflows::handler::default_registry(interviewer.clone(), {
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
    let mut engine = fabro_workflows::engine::WorkflowRunEngine::with_interviewer(
        registry,
        Arc::clone(&emitter),
        interviewer,
        Arc::clone(&sandbox),
    );
    if !sandbox_env.is_empty() {
        engine.set_env(sandbox_env);
    }
    if dry_run_mode {
        engine.set_dry_run(true);
    }
    // Wire up hook runner from run defaults (mirrors run_command)
    {
        let hooks = &run_defaults.hooks;
        if !hooks.is_empty() {
            let hook_config = fabro_hooks::HookConfig {
                hooks: hooks.clone(),
            };
            let runner = fabro_hooks::HookRunner::new(hook_config);
            engine.set_hook_runner(Arc::new(runner));
        }
    }

    let lifecycle = fabro_workflows::engine::LifecycleConfig {
        setup_commands,
        setup_command_timeout_ms: 300_000,
        devcontainer_phases,
    };

    // Defuse the status guard — engine.run() will write "running" and conclusion handles "concluded"
    scopeguard::ScopeGuard::into_inner(status_guard);

    // Safety net: if we panic or return early, best-effort cleanup via spawn (mirrors run_command).
    let preserve = super::run::resolve_preserve_sandbox(args.preserve_sandbox, None, &run_defaults);
    let sandbox_for_cleanup = Arc::clone(&sandbox);
    let cleanup_guard = scopeguard::guard((), move |()| {
        if preserve {
            return;
        }
        let rt = tokio::runtime::Handle::try_current();
        if let Ok(handle) = rt {
            handle.spawn(async move {
                let _ = sandbox_for_cleanup.cleanup().await;
            });
        }
    });

    let run_start = Instant::now();
    let engine_result = engine
        .run_with_lifecycle(&graph, &mut config, lifecycle, Some(&checkpoint))
        .await;
    let run_duration_ms = run_start.elapsed().as_millis() as u64;

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

    // Finish progress bars after retro (retro stage uses the same ProgressUI)
    progress_ui.lock().expect("progress lock poisoned").finish();

    // Write finalize commit with retro.json + final node files (captures last diff.patch)
    write_finalize_commit(&config, &run_dir).await;

    // Auto-create PR on successful completion (mirrors run_command)
    let mut pushed_branch: Option<String> = None;
    let mut pr_url: Option<String> = None;
    if let Some(ref pr_cfg) = config.pull_request {
        if config.dry_run {
            debug!("Skipping PR creation: dry-run mode");
        } else if let Err(ref e) = engine_result {
            debug!(error = %e, "Skipping PR creation: engine returned an error");
        } else if let Ok(ref outcome) = engine_result {
            if !matches!(
                outcome.status,
                StageStatus::Success | StageStatus::PartialSuccess
            ) {
                debug!(status = ?outcome.status, "Skipping PR creation: run status is not success");
            } else {
                let origin_url = fabro_sandbox::daytona::detect_repo_info(
                    original_cwd
                        .as_deref()
                        .unwrap_or_else(|| std::path::Path::new(".")),
                )
                .ok()
                .map(|(url, _)| url);

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
                    if config.git_checkpoint_enabled {
                        pushed_branch = Some(run_branch.clone());
                    }

                    let auto_merge = if pr_cfg.auto_merge {
                        Some(fabro_workflows::pull_request::AutoMergeConfig {
                            merge_strategy: pr_cfg.merge_strategy,
                        })
                    } else {
                        None
                    };

                    match fabro_workflows::pull_request::maybe_open_pull_request(
                        creds,
                        origin,
                        base_branch,
                        run_branch,
                        graph.goal(),
                        &diff,
                        &model,
                        pr_cfg.draft,
                        auto_merge,
                        &run_dir,
                    )
                    .await
                    {
                        Ok(Some(record)) => {
                            emitter.emit(
                                &fabro_workflows::event::WorkflowRunEvent::PullRequestCreated {
                                    pr_url: record.html_url.clone(),
                                    pr_number: record.number,
                                    draft: pr_cfg.draft,
                                },
                            );
                            pr_url = Some(record.html_url.clone());
                            if let Err(e) = record.save(&run_dir.join("pull_request.json")) {
                                tracing::warn!(error = %e, "Failed to save pull_request.json");
                            }
                        }
                        Ok(None) => {} // empty diff, logged at DEBUG
                        Err(e) => {
                            emitter.emit(
                                &fabro_workflows::event::WorkflowRunEvent::PullRequestFailed {
                                    error: e.to_string(),
                                },
                            );
                            eprintln!(
                                "{} PR creation failed: {e}",
                                styles.yellow.apply_to("Warning:")
                            );
                        }
                    }
                }
            }
        }
    } else {
        debug!("Skipping PR creation: pull_request not enabled in config");
    }

    // Defuse the cleanup guard — we are about to do explicit cleanup
    scopeguard::ScopeGuard::into_inner(cleanup_guard);

    // Cleanup sandbox via engine (fires SandboxCleanup hook)
    // Before cleanup, print preserve banner (mirrors run_command)
    if preserve {
        let info = sandbox.sandbox_info();
        if !info.is_empty() {
            eprintln!(
                "\n{} sandbox preserved: {info}",
                styles.bold.apply_to("Info:")
            );
        } else {
            eprintln!("\n{} sandbox preserved", styles.bold.apply_to("Info:"));
        }
    }
    if let Err(e) = engine
        .cleanup_sandbox(&config.run_id, &graph.name, preserve)
        .await
    {
        tracing::warn!(error = %e, "Sandbox cleanup failed");
        eprintln!(
            "\n{} sandbox cleanup failed: {e}",
            styles.yellow.apply_to("Warning:")
        );
    }

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

    {
        use crate::commands::shared::format_tokens_human;
        use fabro_workflows::cost::format_cost;
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
