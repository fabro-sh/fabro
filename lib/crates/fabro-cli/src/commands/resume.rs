use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use clap::Args;
use fabro_agent::{DockerSandbox, DockerSandboxConfig, Sandbox, WorktreeConfig, WorktreeSandbox};
use fabro_config::config::FabroConfig;
use fabro_interview::{AutoApproveInterviewer, ConsoleInterviewer, Interviewer};
use fabro_model::{Catalog, Provider};
use fabro_util::terminal::Styles;
use fabro_workflows::records::Checkpoint;
use fabro_workflows::event::{EventEmitter, RunNoticeLevel};
use fabro_workflows::handler::llm::{AgentApiBackend, AgentCliBackend, BackendRouter};
use fabro_workflows::operations::{
    create_from_graph, start, StartFinalizeConfig, StartOptions, StartRetroConfig,
};
use fabro_workflows::outcome::StageStatus;
use fabro_workflows::pipeline::{
    build_conclusion, classify_engine_result, persist_terminal_outcome,
};
use fabro_workflows::records::RunRecord;
use fabro_workflows::run_settings::{GitCheckpointSettings, LifecycleConfig, RunSettings};
use fabro_workflows::sandbox_provider::SandboxProvider;

use super::detached_support::{DetachedRunBootstrapGuard, DetachedRunCompletionGuard};
use super::run::{
    build_event_envelope, cached_graph_path, default_run_dir, emit_run_notice,
    local_sandbox_with_callback, mint_github_token, prepare_workflow_with_project_config,
    print_assets, print_final_output, print_retro_result, print_run_conclusion,
    resolve_daytona_config, resolve_fallback_chain, resolve_model_provider,
    resolve_ssh_clone_params, resolve_ssh_config, write_run_config_snapshot, CliSandboxProvider,
    RunArgs,
};
use fabro_config::project as project_config;
use fabro_config::run as run_config;
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
    validated: fabro_workflows::pipeline::Validated,
    run_id: String,
    run_dir: PathBuf,
    run_cfg: Option<FabroConfig>,
    sandbox: Arc<dyn Sandbox>,
    /// Kept as Arc so the sandbox event callbacks can emit through it. Listeners
    /// that need to be added later (e.g. ProgressUI) are registered separately.
    emitter: Arc<EventEmitter>,
    settings: RunSettings,
    setup_commands: Vec<String>,
    /// Devcontainer lifecycle phases (on_create, post_create, post_start) resolved from config.
    devcontainer_phases: Vec<(String, Vec<fabro_devcontainer::Command>)>,
    /// Devcontainer remoteEnv values to layer under sandbox_env.
    devcontainer_env: HashMap<String, String>,
    /// Original cwd to restore after engine run (git-branch path changes cwd to worktree).
    original_cwd: Option<PathBuf>,
    origin_url: Option<String>,
    sandbox_provider: SandboxProvider,
    ssh_data_host: Option<String>,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    status_guard: DetachedRunBootstrapGuard,
}

fn resume_as_run_args(args: &ResumeArgs, workflow: PathBuf) -> RunArgs {
    RunArgs {
        workflow: Some(workflow),
        run_dir: None,
        dry_run: args.dry_run,
        preflight: false,
        auto_approve: args.auto_approve,
        goal: None,
        goal_file: None,
        model: args.model.clone(),
        provider: args.provider.clone(),
        verbose: args.verbose,
        sandbox: args.sandbox,
        label: Vec::new(),
        no_retro: args.no_retro,
        preserve_sandbox: args.preserve_sandbox,
        detach: false,
        run_id: None,
    }
}

fn preferred_resume_repo_path(
    original_cwd: &std::path::Path,
    record: Option<&RunRecord>,
) -> PathBuf {
    record
        .and_then(|r| r.host_repo_path.as_deref())
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .unwrap_or_else(|| original_cwd.to_path_buf())
}

/// Resume an interrupted workflow run.
///
/// # Errors
///
/// Returns an error if the run cannot be found, the checkpoint cannot be loaded,
/// or the workflow cannot be resumed.
pub async fn resume_command(
    args: ResumeArgs,
    mut run_defaults: FabroConfig,
    styles: &'static Styles,
    github_app: Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<()> {
    // Apply project-level config overrides (fabro.toml) on top of CLI defaults (mirrors run_command).
    if let Ok(Some((_config_path, project_config))) =
        project_config::discover_project_config(&std::env::current_dir().unwrap_or_default())
    {
        tracing::debug!("Applying run defaults from fabro.toml");
        run_defaults.merge_overlay(project_config);
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
    run_defaults: &FabroConfig,
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
    let prepared = prepare_workflow_with_project_config(
        &resume_as_run_args(args, workflow_path.clone()),
        run_defaults.clone(),
        styles,
        true,
        false,
    )?;
    let source = prepared.raw_source.clone();
    let validated = prepared.validated;
    let graph = validated.graph().clone();
    let run_cfg = prepared.run_cfg;
    let sandbox_provider = prepared.sandbox_provider;
    let workflow_slug = prepared.workflow_slug;
    let prepared_model = prepared.model;
    let prepared_provider = prepared.provider;
    let prepared_run_defaults = prepared.run_defaults;
    let workflow_toml_path = prepared.workflow_toml_path;

    eprintln!(
        "{} {} from checkpoint {}",
        styles.bold.apply_to("Resuming workflow:"),
        graph.name,
        styles.dim.apply_to(checkpoint_path.display()),
    );

    let run_id = ulid::Ulid::new().to_string();
    let run_dir = args
        .run_dir
        .clone()
        .unwrap_or_else(|| default_run_dir(&run_id, args.dry_run));
    tokio::fs::create_dir_all(&run_dir).await?;
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;
    let status_guard = DetachedRunBootstrapGuard::arm(&run_dir)?;
    tokio::fs::write(cached_graph_path(&run_dir), &source).await?;
    let run_cfg: Option<FabroConfig> = run_cfg;
    write_run_config_snapshot(&run_dir, workflow_toml_path.as_deref()).await?;

    // Write RunRecord for the resumed run
    let settings_config = {
        let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let cli_flags = super::create::CliFlags {
            dry_run: args.dry_run,
            auto_approve: args.auto_approve,
            no_retro: args.no_retro,
            verbose: args.verbose,
            preserve_sandbox: args.preserve_sandbox,
        };
        let normalized = super::create::normalize_config(
            run_cfg.as_ref(),
            &prepared_run_defaults,
            &prepared_model,
            prepared_provider.as_deref(),
            sandbox_provider,
            &graph,
            cli_flags,
        );
        let record = fabro_workflows::records::RunRecord {
            run_id: run_id.clone(),
            created_at: chrono::Utc::now(),
            config: normalized.clone(),
            graph: graph.clone(),
            workflow_slug: workflow_slug.clone(),
            working_directory: working_directory.clone(),
            host_repo_path: Some(working_directory.to_string_lossy().to_string()),
            base_branch: None,
            labels: std::collections::HashMap::new(),
        };
        let _ = record.save(&run_dir);
        normalized
    };

    let original_cwd = std::env::current_dir()?;
    let emitter = Arc::new(EventEmitter::new());

    // Resolve devcontainer BEFORE sandbox creation (mirrors run_command) so that
    // the Daytona snapshot config can be overridden with the devcontainer Dockerfile.
    let run_defaults = &run_defaults;
    let mut daytona_config = resolve_daytona_config(run_cfg.as_ref(), run_defaults);
    let devcontainer_config = if run_cfg
        .as_ref()
        .and_then(|c| c.sandbox.as_ref())
        .or(run_defaults.sandbox.as_ref())
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
            let exe_config = super::run::resolve_exe_config(run_cfg.as_ref(), run_defaults);
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
            let config = resolve_ssh_config(run_cfg.as_ref(), run_defaults)
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

    let settings = RunSettings {
        config: settings_config,
        run_dir: run_dir.clone(),
        cancel_token: None,
        dry_run: args.dry_run,
        run_id: run_id.clone(),
        host_repo_path: None,
        git: None,
        labels: args
            .label
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        github_app: github_app.clone(),
        git_author,
        base_branch: None,
        workflow_slug,
    };

    let devcontainer_env = devcontainer_config
        .as_ref()
        .map(|dc| dc.environment.clone())
        .unwrap_or_default();
    let setup_commands = run_cfg
        .as_ref()
        .and_then(|cfg| cfg.setup.as_ref())
        .or(run_defaults.setup.as_ref())
        .map(|s| s.commands.clone())
        .unwrap_or_default();

    Ok(ResumeContext {
        checkpoint,
        validated,
        run_id,
        run_dir,
        run_cfg,
        sandbox,
        emitter,
        settings,
        setup_commands,
        devcontainer_phases,
        devcontainer_env,
        original_cwd: None,
        origin_url: fabro_sandbox::daytona::detect_repo_info(&original_cwd)
            .ok()
            .map(|(url, _)| url),
        sandbox_provider,
        ssh_data_host,
        github_app: github_app.clone(),
        status_guard,
    })
}

/// Git-branch path: resolve run ID, read checkpoint + graph from metadata, set up worktree.
async fn prepare_from_branch(
    args: &ResumeArgs,
    styles: &Styles,
    run_defaults: &FabroConfig,
    github_app: &Option<fabro_github::GitHubAppCredentials>,
    git_author: fabro_workflows::git::GitAuthor,
) -> anyhow::Result<ResumeContext> {
    let run_arg = args.run.as_deref().expect("run is required");

    let (run_id, run_branch) =
        if let Some(stripped) = run_arg.strip_prefix(fabro_workflows::git::RUN_BRANCH_PREFIX) {
            (stripped.to_string(), run_arg.to_string())
        } else {
            let repo = git2::Repository::discover(".").context("not in a git repository")?;
            let id = fabro_workflows::operations::find_run_id_by_prefix(&repo, run_arg)?;
            let branch = format!("{}{}", fabro_workflows::git::RUN_BRANCH_PREFIX, id);
            (id, branch)
        };

    let original_cwd = std::env::current_dir()?;
    let record_hint = fabro_workflows::git::MetadataStore::read_run_record(&original_cwd, &run_id)
        .ok()
        .flatten();
    let resume_repo_path = preferred_resume_repo_path(&original_cwd, record_hint.as_ref());
    let record = if resume_repo_path == original_cwd {
        record_hint
    } else {
        fabro_workflows::git::MetadataStore::read_run_record(&resume_repo_path, &run_id)
            .ok()
            .flatten()
            .or(record_hint)
    };
    let start_record =
        fabro_workflows::git::MetadataStore::read_start_record(&resume_repo_path, &run_id)
            .ok()
            .flatten();
    let checkpoint =
        fabro_workflows::git::MetadataStore::read_checkpoint(&resume_repo_path, &run_id)?
            .ok_or_else(|| {
                anyhow::anyhow!("no checkpoint found on metadata branch for run {run_id}")
            })?;
    let repo_info = fabro_sandbox::daytona::detect_repo_info(&resume_repo_path).ok();
    let origin_url = repo_info.as_ref().map(|(url, _)| url.clone());
    let detected_base_branch = record
        .as_ref()
        .and_then(|r| r.base_branch.clone())
        .or_else(|| repo_info.as_ref().and_then(|(_, branch)| branch.clone()));
    let base_sha = start_record.as_ref().and_then(|s| s.base_sha.clone());

    let (validated, graph_source, run_cfg, mut sandbox_provider, workflow_slug) =
        if let Some(ref workflow_path) = args.workflow {
            let prepared = prepare_workflow_with_project_config(
                &resume_as_run_args(args, workflow_path.clone()),
                run_defaults.clone(),
                styles,
                true,
                false,
            )?;
            (
                prepared.validated,
                prepared.raw_source,
                prepared.run_cfg,
                prepared.sandbox_provider,
                prepared.workflow_slug,
            )
        } else if let Some(ref rec) = record {
            // Use the fully transformed graph from the RunRecord
            let validated = create_from_graph(rec.graph.clone(), String::new());
            let source = String::new(); // no DOT source needed — graph is from RunRecord
            let run_cfg = Some(rec.config.clone());
            let sandbox_provider = if args.dry_run {
                SandboxProvider::Local
            } else {
                let sp = rec
                    .config
                    .sandbox
                    .as_ref()
                    .and_then(|s| s.provider.as_deref())
                    .and_then(|s| s.parse::<SandboxProvider>().ok())
                    .unwrap_or_default();
                args.sandbox.map(Into::into).unwrap_or(sp)
            };
            (
                validated,
                source,
                run_cfg,
                sandbox_provider,
                rec.workflow_slug.clone(),
            )
        } else {
            bail!("no run.json found on metadata branch for run {run_id}");
        };
    let graph = validated.graph().clone();

    eprintln!(
        "{} {} from branch {} ({})",
        styles.bold.apply_to("Resuming workflow:"),
        graph.name,
        styles.dim.apply_to(&run_branch),
        run_id,
    );

    // Set up logs directory — reuse existing run dir for this run_id to avoid
    // "ambiguous prefix" errors when the resume happens on a different day.
    // Skip reuse when dry-running to avoid corrupting real run data.
    let run_dir = if let Some(ref dir) = args.run_dir {
        dir.clone()
    } else if args.dry_run {
        default_run_dir(&run_id, true)
    } else {
        find_existing_run_dir(&run_id, false).unwrap_or_else(|| default_run_dir(&run_id, false))
    };
    tokio::fs::create_dir_all(&run_dir).await?;
    let run_dir = tokio::fs::canonicalize(&run_dir).await.unwrap_or(run_dir);
    fabro_util::run_log::activate(&run_dir.join("cli.log"))
        .context("Failed to activate per-run log")?;
    let status_guard = DetachedRunBootstrapGuard::arm(&run_dir)?;
    if !graph_source.is_empty() {
        tokio::fs::write(cached_graph_path(&run_dir), &graph_source).await?;
    }
    let run_cfg: Option<FabroConfig> = run_cfg;
    // Git-branch resume: no original TOML available, skip debug snapshot.
    write_run_config_snapshot(&run_dir, None).await?;

    // Write RunRecord for the resumed run
    let settings_config = {
        let (model_str, provider_str) = resolve_model_provider(
            args.model.as_deref(),
            args.provider.as_deref(),
            run_cfg.as_ref(),
            run_defaults,
            &graph,
        );
        let cli_flags = super::create::CliFlags {
            dry_run: args.dry_run,
            auto_approve: args.auto_approve,
            no_retro: args.no_retro,
            verbose: args.verbose,
            preserve_sandbox: args.preserve_sandbox,
        };
        let normalized = super::create::normalize_config(
            run_cfg.as_ref(),
            run_defaults,
            &model_str,
            provider_str.as_deref(),
            sandbox_provider,
            &graph,
            cli_flags,
        );
        let record = fabro_workflows::records::RunRecord {
            run_id: run_id.clone(),
            created_at: chrono::Utc::now(),
            config: normalized.clone(),
            graph: graph.clone(),
            workflow_slug: workflow_slug.clone(),
            working_directory: resume_repo_path.clone(),
            host_repo_path: Some(resume_repo_path.to_string_lossy().to_string()),
            base_branch: detected_base_branch.clone(),
            labels: std::collections::HashMap::new(),
        };
        let _ = record.save(&run_dir);
        normalized
    };

    let emitter = Arc::new(EventEmitter::new());

    // Resolve devcontainer BEFORE sandbox creation (mirrors run_command) so that
    // the Daytona snapshot config can be overridden with the devcontainer Dockerfile.
    let mut daytona_config = resolve_daytona_config(run_cfg.as_ref(), run_defaults);
    let devcontainer_config = if run_cfg
        .as_ref()
        .and_then(|cfg| cfg.sandbox.as_ref())
        .or(run_defaults.sandbox.as_ref())
        .and_then(|s| s.devcontainer)
        .unwrap_or(false)
    {
        match fabro_devcontainer::DevcontainerResolver::resolve(&resume_repo_path).await {
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
                            .current_dir(&resume_repo_path)
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

        let inner = local_sandbox_with_callback(resume_repo_path.clone(), Arc::clone(emitter));
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
            let exe_config = super::run::resolve_exe_config(run_cfg.as_ref(), run_defaults);
            let clone_params = super::run::resolve_exe_clone_params(&resume_repo_path);
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
            let config = resolve_ssh_config(run_cfg.as_ref(), run_defaults)
                .ok_or_else(|| anyhow::anyhow!("--sandbox ssh requires [sandbox.ssh] config"))?;
            ssh_data_host = Some(config.destination.clone());
            let clone_params = resolve_ssh_clone_params(&resume_repo_path);
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
    let mut setup_commands: Vec<String> = run_cfg
        .as_ref()
        .and_then(|cfg| cfg.setup.as_ref())
        .or(run_defaults.setup.as_ref())
        .map(|s| s.commands.clone())
        .unwrap_or_default();
    setup_commands.extend(sandbox.resume_setup_commands(&run_branch));

    let settings = RunSettings {
        config: settings_config,
        run_dir: run_dir.clone(),
        cancel_token: None,
        dry_run: args.dry_run,
        run_id: run_id.clone(),
        host_repo_path: Some(resume_repo_path.clone()),
        git: Some(GitCheckpointSettings {
            base_sha,
            run_branch: Some(run_branch),
            meta_branch: Some(fabro_workflows::git::MetadataStore::branch_name(&run_id)),
        }),
        labels: args
            .label
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        github_app: github_app.clone(),
        git_author,
        base_branch: detected_base_branch,
        workflow_slug,
    };

    let devcontainer_env = devcontainer_config
        .as_ref()
        .map(|dc| dc.environment.clone())
        .unwrap_or_default();

    Ok(ResumeContext {
        checkpoint,
        validated,
        run_id,
        run_dir,
        run_cfg,
        sandbox,
        emitter,
        settings,
        setup_commands,
        devcontainer_phases,
        devcontainer_env,
        original_cwd: Some(original_cwd),
        origin_url,
        sandbox_provider,
        ssh_data_host,
        github_app: github_app.clone(),
        status_guard,
    })
}

/// Shared tail: build engine, run workflow, generate retro, print results.
async fn run_resumed(
    ctx: ResumeContext,
    args: ResumeArgs,
    run_defaults: FabroConfig,
    styles: &'static Styles,
) -> anyhow::Result<()> {
    let ResumeContext {
        checkpoint,
        validated,
        run_id,
        run_dir,
        mut run_cfg,
        sandbox,
        emitter,
        mut settings,
        setup_commands,
        devcontainer_phases,
        devcontainer_env,
        original_cwd,
        origin_url,
        sandbox_provider,
        ssh_data_host,
        github_app,
        mut status_guard,
    } = ctx;
    let graph = validated.graph().clone();

    // Create progress UI (verbose mode shows detailed turn/tool counts and token usage)
    let is_tty = std::io::stderr().is_terminal();
    let progress_ui = Arc::new(std::sync::Mutex::new(super::run_progress::ProgressUI::new(
        is_tty,
        args.verbose,
    )));
    {
        let mut ui = progress_ui.lock().expect("progress lock poisoned");
        ui.show_version();
        ui.show_run_id(&run_id);
        ui.show_time(&chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
        ui.show_run_dir(&run_dir);
    }
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
                let record = fabro_workflows::records::SandboxRecord {
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
                emit_run_notice(
                    &emitter,
                    RunNoticeLevel::Warn,
                    "dry_run_no_llm",
                    "No LLM providers configured. Running in dry-run mode.",
                );
                true
            }
            Ok(_) => false,
            Err(e) => {
                emit_run_notice(
                    &emitter,
                    RunNoticeLevel::Warn,
                    "dry_run_llm_init_failed",
                    format!("Failed to initialize LLM client: {e}. Running in dry-run mode."),
                );
                true
            }
        }
    };
    settings.dry_run = dry_run_mode;

    if let Some(ref mut cfg) = run_cfg {
        run_config::resolve_sandbox_env(cfg)?;
    }

    let (model, provider) = resolve_model_provider(
        args.model.as_deref(),
        args.provider.as_deref(),
        run_cfg.as_ref(),
        &run_defaults,
        &graph,
    );
    let provider_enum: Provider = provider
        .as_deref()
        .map(|s| s.parse::<Provider>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or_else(Provider::default_from_env);

    let fallback_chain = if run_cfg.is_some() {
        resolve_fallback_chain(provider_enum, &model, run_cfg.as_ref())
    } else {
        match run_defaults.llm.as_ref().and_then(|l| l.fallbacks.as_ref()) {
            Some(map) => Catalog::builtin().build_fallback_chain(provider_enum, &model, map),
            None => Vec::new(),
        }
    };

    // Build sandbox env: devcontainer env layered underneath TOML env (TOML wins on conflict, mirrors run_command)
    let sandbox_env: HashMap<String, String> = {
        let mut env = devcontainer_env;
        if let Some(mut toml_env) = run_cfg
            .as_ref()
            .and_then(|cfg| cfg.sandbox.as_ref())
            .and_then(|s| s.env.clone())
            .or_else(|| run_defaults.sandbox.as_ref().and_then(|s| s.env.clone()))
        {
            run_config::resolve_env_refs(&mut toml_env)?;
            env.extend(toml_env);
        }
        env
    };

    // Mint a GitHub App IAT and inject as GITHUB_TOKEN if [github] permissions are declared
    let mut sandbox_env = sandbox_env;
    let github_permissions = run_cfg
        .as_ref()
        .and_then(|cfg| cfg.github.as_ref())
        .or(run_defaults.github.as_ref());
    if let Some(gh_cfg) = github_permissions {
        if !gh_cfg.permissions.is_empty() {
            if let (Some(ref creds), Some(ref url)) = (&github_app, &origin_url) {
                match mint_github_token(creds, url, &gh_cfg.permissions).await {
                    Ok(token) => {
                        debug!("Minted GitHub IAT for sandbox GITHUB_TOKEN");
                        sandbox_env.insert("GITHUB_TOKEN".to_string(), token);
                    }
                    Err(e) => {
                        emit_run_notice(
                            &emitter,
                            RunNoticeLevel::Warn,
                            "github_token_failed",
                            format!("Failed to mint GitHub token: {e}"),
                        );
                    }
                }
            } else {
                debug!("Skipping GitHub token: no GitHub App credentials or origin URL");
            }
        }
    }

    // Resolve MCP servers from run defaults
    let mcp_servers: Vec<fabro_mcp::config::McpServerConfig> = run_cfg
        .as_ref()
        .map(|cfg| cfg.mcp_servers.clone())
        .unwrap_or_else(|| run_defaults.mcp_servers.clone())
        .clone()
        .into_iter()
        .map(|(name, entry): (String, fabro_config::mcp::McpServerEntry)| entry.into_config(name))
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
    let lifecycle = LifecycleConfig {
        setup_commands,
        setup_command_timeout_ms: 300_000,
        devcontainer_phases,
    };

    // Defuse the bootstrap guard — engine.run() has taken ownership of lifecycle status.
    status_guard.defuse();

    let preserve = super::run::resolve_preserve_sandbox(
        args.preserve_sandbox,
        run_cfg.as_ref(),
        &run_defaults,
    );
    let run_start = Instant::now();
    let pr_config = if dry_run_mode {
        None
    } else {
        settings.pull_request().cloned()
    };
    let started = start(
        validated,
        StartOptions {
            init: fabro_workflows::pipeline::InitOptions {
                run_id: run_id.clone(),
                run_dir: run_dir.clone(),
                dry_run: dry_run_mode,
                emitter: Arc::clone(&emitter),
                sandbox: Arc::clone(&sandbox),
                registry: Arc::new(registry),
                lifecycle,
                run_settings: settings,
                hooks: fabro_hooks::HookConfig {
                    hooks: run_cfg
                        .as_ref()
                        .map(|cfg| cfg.hooks.clone())
                        .unwrap_or_else(|| run_defaults.hooks.clone()),
                },
                sandbox_env,
                checkpoint: Some(checkpoint),
                seed_context: None,
            },
            retro: StartRetroConfig {
                enabled: !args.no_retro && project_config::is_retro_enabled(),
                dry_run: dry_run_mode,
                llm_client: if dry_run_mode {
                    None
                } else {
                    fabro_llm::client::Client::from_env().await.ok()
                },
                provider: provider_enum,
                model: model.clone(),
            },
            finalize: StartFinalizeConfig {
                preserve_sandbox: preserve,
                pr_config,
                github_app: github_app.clone(),
                origin_url: origin_url.clone(),
                model: model.clone(),
            },
        },
    )
    .await;
    let run_duration_ms = run_start.elapsed().as_millis() as u64;
    let mut completion_guard = DetachedRunCompletionGuard::arm(&run_dir);

    // Restore cwd if we changed it (worktree is kept for `fabro cp` access; pruned separately)
    if let Some(ref cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }

    progress_ui.lock().expect("progress lock poisoned").finish();
    let final_status = match started {
        Ok(started) => {
            if let Some(ref retro) = started.retro {
                print_retro_result(retro, started.retro_duration, &run_dir, styles);
            } else if !args.no_retro && project_config::is_retro_enabled() {
                eprintln!("\n{}", styles.bold.apply_to("=== Retro ==="));
                eprintln!("{}", styles.dim.apply_to("Retro unavailable"));
            }
            let finalized = started.finalized;
            print_run_conclusion(
                &finalized.conclusion,
                &run_id,
                &run_dir,
                finalized.pushed_branch.as_deref(),
                finalized.pr_url.as_deref(),
                styles,
            );
            print_final_output(&run_dir, styles);
            print_assets(&run_dir, styles);
            finalized.conclusion.status.clone()
        }
        Err(err) => {
            let engine_result: Result<fabro_workflows::outcome::Outcome, _> = Err(err.clone());
            let (final_status, failure_reason, run_status, status_reason) =
                classify_engine_result(&engine_result);
            let conclusion = build_conclusion(
                &run_dir,
                final_status.clone(),
                failure_reason,
                run_duration_ms,
                None,
            );
            persist_terminal_outcome(&run_dir, &conclusion, run_status, status_reason);
            print_run_conclusion(&conclusion, &run_id, &run_dir, None, None, styles);
            print_final_output(&run_dir, styles);
            print_assets(&run_dir, styles);
            final_status
        }
    };

    completion_guard.defuse();

    fabro_util::run_log::deactivate();
    match final_status {
        StageStatus::Success | StageStatus::PartialSuccess => Ok(()),
        _ => std::process::exit(1),
    }
}

/// Scan a runs directory for an existing directory matching the requested dry-run mode.
fn find_existing_run_dir_in(
    base: &std::path::Path,
    run_id: &str,
    dry_run: bool,
) -> Option<PathBuf> {
    let suffix = format!("-{run_id}");
    let entries = std::fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.path().is_dir() && run_dir_name_matches_mode(&name, &suffix, dry_run) {
            return Some(entry.path());
        }
    }
    None
}

/// Scan `~/.fabro/runs/` for an existing directory matching the requested dry-run mode.
fn find_existing_run_dir(run_id: &str, dry_run: bool) -> Option<PathBuf> {
    let base = dirs::home_dir()?.join(".fabro").join("runs");
    find_existing_run_dir_in(&base, run_id, dry_run)
}

fn run_dir_name_matches_mode(name: &str, run_id_suffix: &str, dry_run: bool) -> bool {
    name.strip_suffix(run_id_suffix)
        .is_some_and(|prefix| prefix.ends_with("-dry-run") == dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use fabro_workflows::run_status::{RunStatus, RunStatusRecord, StatusReason};

    fn sample_run_record() -> RunRecord {
        RunRecord {
            run_id: "run-1".to_string(),
            created_at: Utc::now(),
            config: fabro_config::config::FabroConfig::default(),
            graph: fabro_graphviz::graph::Graph {
                name: "resume".to_string(),
                ..Default::default()
            },
            workflow_slug: None,
            working_directory: std::path::PathBuf::from("/tmp"),
            host_repo_path: None,
            base_branch: Some("main".to_string()),
            labels: HashMap::new(),
        }
    }

    #[test]
    fn preferred_resume_repo_path_uses_record_host_repo_path_when_present() {
        let cwd = tempfile::tempdir().unwrap();
        let host_repo = tempfile::tempdir().unwrap();
        let mut record = sample_run_record();
        record.host_repo_path = Some(host_repo.path().to_string_lossy().to_string());

        let selected = preferred_resume_repo_path(cwd.path(), Some(&record));
        assert_eq!(selected, host_repo.path());
    }

    #[test]
    fn preferred_resume_repo_path_falls_back_when_record_path_is_missing() {
        let cwd = tempfile::tempdir().unwrap();
        let mut record = sample_run_record();
        record.host_repo_path = Some(cwd.path().join("missing-repo").display().to_string());

        let selected = preferred_resume_repo_path(cwd.path(), Some(&record));
        assert_eq!(selected, cwd.path());
    }

    #[test]
    fn find_existing_run_dir_in_respects_dry_run_mode() {
        let runs = tempfile::tempdir().unwrap();
        let non_dry = runs.path().join("20260323-run-1");
        let dry = runs.path().join("20260323-dry-run-run-1");
        std::fs::create_dir_all(&non_dry).unwrap();
        std::fs::create_dir_all(&dry).unwrap();

        assert_eq!(
            find_existing_run_dir_in(runs.path(), "run-1", false),
            Some(non_dry)
        );
        assert_eq!(
            find_existing_run_dir_in(runs.path(), "run-1", true),
            Some(dry)
        );
    }

    #[test]
    fn find_existing_run_dir_in_does_not_misclassify_run_ids_containing_dry_run() {
        let runs = tempfile::tempdir().unwrap();
        let run_id = "feature-dry-run-fix";
        let non_dry = runs.path().join(format!("20260323-{run_id}"));
        let dry = runs.path().join(format!("20260323-dry-run-{run_id}"));
        std::fs::create_dir_all(&non_dry).unwrap();
        std::fs::create_dir_all(&dry).unwrap();

        assert_eq!(
            find_existing_run_dir_in(runs.path(), run_id, false),
            Some(non_dry)
        );
        assert_eq!(
            find_existing_run_dir_in(runs.path(), run_id, true),
            Some(dry)
        );
    }

    #[test]
    fn resume_bootstrap_guard_marks_failed_on_drop() {
        let dir = tempfile::tempdir().unwrap();

        {
            let guard = DetachedRunBootstrapGuard::arm(dir.path()).unwrap();
            let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
            assert_eq!(record.status, RunStatus::Starting);
            assert_eq!(record.reason, Some(StatusReason::SandboxInitializing));
            drop(guard);
        }

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(record.status, RunStatus::Failed);
        assert_eq!(record.reason, Some(StatusReason::SandboxInitFailed));
        assert!(dir.path().join("run.pid").exists());
    }

    #[test]
    fn resume_bootstrap_guard_does_not_overwrite_after_defuse() {
        let dir = tempfile::tempdir().unwrap();
        let mut guard = DetachedRunBootstrapGuard::arm(dir.path()).unwrap();
        guard.defuse();
        drop(guard);

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(record.status, RunStatus::Starting);
        assert_eq!(record.reason, Some(StatusReason::SandboxInitializing));
    }

    #[test]
    fn resume_completion_guard_marks_failed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id.txt"), "run-resume").unwrap();

        {
            let _guard = DetachedRunCompletionGuard::arm(dir.path());
        }

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(record.status, RunStatus::Failed);
        assert_eq!(record.reason, Some(StatusReason::WorkflowError));
        assert!(dir.path().join("conclusion.json").exists());
    }
}
