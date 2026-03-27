use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fabro_agent::Sandbox;
use fabro_config::sandbox::WorktreeMode;
use fabro_hooks::{HookContext, HookDecision, HookEvent, HookRunner};
use fabro_llm::client::Client;
use fabro_sandbox::{
    DockerSandbox, LocalSandbox, ReadBeforeWriteSandbox, SandboxRecord, WorktreeConfig,
    WorktreeSandbox,
};
use shlex::try_quote;

use crate::error::FabroError;
use crate::event::{RunNoticeLevel, WorkflowRunEvent};
use crate::git::{self, GitSyncStatus};
use crate::handler::default_registry;
use crate::handler::llm::{AgentApiBackend, AgentCliBackend, BackendRouter};
use crate::run_options::GitCheckpointOptions;

use super::types::{InitOptions, Initialized, LlmSpec, Persisted, SandboxEnvSpec, SandboxSpec};

struct SandboxBuildResult {
    sandbox: Arc<dyn Sandbox>,
    worktree_created: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkdirStrategy {
    LocalDirectory,
    LocalWorktree,
    Cloud,
}

struct WorktreePlan {
    branch_name: String,
    base_sha: String,
    worktree_path: PathBuf,
}

async fn run_hooks(
    hook_runner: Option<&HookRunner>,
    hook_context: &HookContext,
    sandbox: Arc<dyn Sandbox>,
    work_dir: Option<&Path>,
) -> HookDecision {
    let Some(runner) = hook_runner else {
        return HookDecision::Proceed;
    };
    runner.run(hook_context, sandbox, work_dir).await
}

fn emit_run_notice(
    emitter: &crate::event::EventEmitter,
    level: RunNoticeLevel,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    emitter.emit(&WorkflowRunEvent::RunNotice {
        level,
        code: code.into(),
        message: message.into(),
    });
}

fn sandbox_provider_name(spec: &SandboxSpec) -> &'static str {
    match spec {
        SandboxSpec::Local { .. } => "local",
        SandboxSpec::Docker { .. } => "docker",
        SandboxSpec::Daytona { .. } => "daytona",
        #[cfg(feature = "exedev")]
        SandboxSpec::Exe { .. } => "exe",
        SandboxSpec::Ssh { .. } => "ssh",
    }
}

fn host_repo_path_for_planning(
    run_options: &crate::run_options::RunOptions,
    spec: &SandboxSpec,
) -> Option<PathBuf> {
    run_options.host_repo_path.clone().or_else(|| match spec {
        SandboxSpec::Local { working_directory } => Some(working_directory.clone()),
        SandboxSpec::Docker { config } => Some(PathBuf::from(&config.host_working_directory)),
        _ => None,
    })
}

fn resolve_workdir_strategy(
    spec: &SandboxSpec,
    worktree_mode: WorktreeMode,
    git_status: GitSyncStatus,
    checkpoint_present: bool,
) -> WorkdirStrategy {
    if checkpoint_present {
        return match spec {
            SandboxSpec::Local { .. } | SandboxSpec::Docker { .. } => {
                WorkdirStrategy::LocalDirectory
            }
            _ => WorkdirStrategy::Cloud,
        };
    }

    match spec {
        SandboxSpec::Local { .. } => match worktree_mode {
            WorktreeMode::Always => WorkdirStrategy::LocalWorktree,
            WorktreeMode::Clean => {
                if git_status.is_clean() {
                    WorkdirStrategy::LocalWorktree
                } else {
                    WorkdirStrategy::LocalDirectory
                }
            }
            WorktreeMode::Dirty => {
                if git_status.is_clean() {
                    WorkdirStrategy::LocalDirectory
                } else {
                    WorkdirStrategy::LocalWorktree
                }
            }
            WorktreeMode::Never => WorkdirStrategy::LocalDirectory,
        },
        SandboxSpec::Docker { .. } => WorkdirStrategy::LocalDirectory,
        _ => WorkdirStrategy::Cloud,
    }
}

async fn resolve_worktree_plan(
    options: &mut InitOptions,
) -> Result<Option<WorktreePlan>, FabroError> {
    let Some(worktree_mode) = options.worktree_mode else {
        options.run_options.display_base_sha = None;
        return Ok(None);
    };

    let host_repo_path = host_repo_path_for_planning(&options.run_options, &options.sandbox);
    let git_status = host_repo_path
        .as_ref()
        .map(|path| git::sync_status(path, "origin", options.run_options.base_branch.as_deref()))
        .unwrap_or(GitSyncStatus::Dirty);
    let strategy = resolve_workdir_strategy(
        &options.sandbox,
        worktree_mode,
        git_status,
        options.checkpoint.is_some(),
    );

    if git_status == GitSyncStatus::Dirty {
        let env_name = match strategy {
            WorkdirStrategy::LocalWorktree => Some("worktree"),
            WorkdirStrategy::Cloud => Some("remote sandbox"),
            WorkdirStrategy::LocalDirectory => None,
        };
        if let Some(env_name) = env_name {
            emit_run_notice(
                &options.emitter,
                RunNoticeLevel::Warn,
                "dirty_worktree",
                format!("Uncommitted changes will not be included in the {env_name}."),
            );
        }
    }

    if !options.dry_run
        && matches!(
            strategy,
            WorkdirStrategy::LocalWorktree | WorkdirStrategy::Cloud
        )
    {
        if let (Some(repo_path), Some(branch)) = (
            host_repo_path.as_ref(),
            options.run_options.base_branch.as_ref(),
        ) {
            let needs_push = match git_status {
                GitSyncStatus::Synced => false,
                GitSyncStatus::Unsynced => true,
                GitSyncStatus::Dirty => {
                    let repo_path = repo_path.clone();
                    let branch = branch.clone();
                    tokio::task::spawn_blocking(move || {
                        git::branch_needs_push(&repo_path, "origin", &branch)
                    })
                    .await
                    .unwrap_or(true)
                }
            };

            if needs_push {
                let repo_path = repo_path.clone();
                let branch = branch.clone();
                let branch_for_push = branch.clone();
                match git::blocking_push_with_timeout(60, move || {
                    git::push_branch(&repo_path, "origin", &branch_for_push)
                })
                .await
                {
                    Ok(()) => emit_run_notice(
                        &options.emitter,
                        RunNoticeLevel::Info,
                        "git_push_succeeded",
                        format!("{branch} (synced local commits to remote)"),
                    ),
                    Err(e) => emit_run_notice(
                        &options.emitter,
                        RunNoticeLevel::Warn,
                        "git_push_failed",
                        format!("Failed to push {branch} to origin: {e}"),
                    ),
                }
            }
        }
    }

    match strategy {
        WorkdirStrategy::LocalWorktree => {
            let Some(repo_path) = host_repo_path else {
                options.run_options.display_base_sha = None;
                return Ok(None);
            };
            match git::head_sha(&repo_path) {
                Ok(base_sha) => {
                    options.run_options.display_base_sha = Some(base_sha.clone());
                    Ok(Some(WorktreePlan {
                        branch_name: format!("{}{}", git::RUN_BRANCH_PREFIX, options.run_id),
                        base_sha,
                        worktree_path: options.run_options.run_dir.join("worktree"),
                    }))
                }
                Err(e) => {
                    emit_run_notice(
                        &options.emitter,
                        RunNoticeLevel::Warn,
                        "worktree_setup_failed",
                        format!("Git worktree setup failed ({e}), running without worktree."),
                    );
                    options.run_options.display_base_sha = None;
                    Ok(None)
                }
            }
        }
        WorkdirStrategy::Cloud => {
            options.run_options.display_base_sha = host_repo_path
                .as_ref()
                .and_then(|path| git::head_sha(path).ok());
            Ok(None)
        }
        WorkdirStrategy::LocalDirectory => {
            options.run_options.display_base_sha = None;
            Ok(None)
        }
    }
}

fn local_sandbox_with_callback(
    working_directory: PathBuf,
    emitter: Arc<crate::event::EventEmitter>,
) -> Arc<dyn Sandbox> {
    let mut sandbox = LocalSandbox::new(working_directory);
    sandbox.set_event_callback(Arc::new(move |event| {
        emitter.emit(&WorkflowRunEvent::Sandbox { event });
    }));
    Arc::new(sandbox)
}

async fn build_sandbox(
    spec: &SandboxSpec,
    worktree_plan: Option<&WorktreePlan>,
    emitter: Arc<crate::event::EventEmitter>,
) -> Result<SandboxBuildResult, FabroError> {
    let mut worktree_created = false;
    let sandbox: Arc<dyn Sandbox> = match spec {
        SandboxSpec::Local { working_directory } => {
            if let Some(plan) = worktree_plan {
                let inner =
                    local_sandbox_with_callback(working_directory.clone(), Arc::clone(&emitter));
                let mut worktree = WorktreeSandbox::new(
                    inner,
                    WorktreeConfig {
                        branch_name: plan.branch_name.clone(),
                        base_sha: plan.base_sha.clone(),
                        worktree_path: plan.worktree_path.to_string_lossy().into_owned(),
                        skip_branch_creation: false,
                    },
                );
                worktree.set_event_callback(Arc::clone(&emitter).worktree_callback());
                match worktree.initialize().await {
                    Ok(()) => {
                        worktree_created = true;
                        Arc::new(ReadBeforeWriteSandbox::new(Arc::new(worktree)))
                    }
                    Err(e) => {
                        emit_run_notice(
                            &emitter,
                            RunNoticeLevel::Warn,
                            "worktree_setup_failed",
                            format!("Git worktree setup failed ({e}), running without worktree."),
                        );
                        Arc::new(ReadBeforeWriteSandbox::new(local_sandbox_with_callback(
                            working_directory.clone(),
                            Arc::clone(&emitter),
                        )))
                    }
                }
            } else {
                Arc::new(ReadBeforeWriteSandbox::new(local_sandbox_with_callback(
                    working_directory.clone(),
                    Arc::clone(&emitter),
                )))
            }
        }
        SandboxSpec::Docker { config } => {
            let mut sandbox = DockerSandbox::new(fabro_sandbox::docker::DockerSandboxConfig {
                image: config.image.clone(),
                host_working_directory: config.host_working_directory.clone(),
                container_mount_point: config.container_mount_point.clone(),
                network_mode: config.network_mode.clone(),
                extra_mounts: config.extra_mounts.clone(),
                memory_limit: config.memory_limit,
                cpu_quota: config.cpu_quota,
                auto_pull: config.auto_pull,
                env_vars: config.env_vars.clone(),
            })
            .map_err(|e| FabroError::engine(format!("Failed to create Docker sandbox: {e}")))?;
            let emitter_cb = Arc::clone(&emitter);
            sandbox.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(ReadBeforeWriteSandbox::new(Arc::new(sandbox)))
        }
        SandboxSpec::Daytona {
            config,
            github_app,
            run_id,
            clone_branch,
        } => {
            let mut sandbox = fabro_sandbox::daytona::DaytonaSandbox::new(
                config.clone(),
                github_app.clone(),
                run_id.clone(),
                clone_branch.clone(),
            )
            .await
            .map_err(FabroError::engine)?;
            let emitter_cb = Arc::clone(&emitter);
            sandbox.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(ReadBeforeWriteSandbox::new(Arc::new(sandbox)))
        }
        #[cfg(feature = "exedev")]
        SandboxSpec::Exe {
            config,
            clone_params,
            run_id,
            github_app,
            mgmt_destination,
        } => {
            let mgmt_ssh = fabro_sandbox::exe::OpensshRunner::connect_raw(mgmt_destination)
                .await
                .map_err(|e| {
                    FabroError::engine(format!("Failed to connect to {mgmt_destination}: {e}"))
                })?;
            let mut sandbox = fabro_sandbox::exe::ExeSandbox::new(
                Box::new(mgmt_ssh),
                config.clone(),
                clone_params.clone(),
                run_id.clone(),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            sandbox.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(ReadBeforeWriteSandbox::new(Arc::new(sandbox)))
        }
        SandboxSpec::Ssh {
            config,
            clone_params,
            run_id,
            github_app,
        } => {
            let mut sandbox = fabro_sandbox::ssh::SshSandbox::new(
                config.clone(),
                clone_params.clone(),
                run_id.clone(),
                github_app.clone(),
            );
            let emitter_cb = Arc::clone(&emitter);
            sandbox.set_event_callback(Arc::new(move |event| {
                emitter_cb.emit(&WorkflowRunEvent::Sandbox { event });
            }));
            Arc::new(ReadBeforeWriteSandbox::new(Arc::new(sandbox)))
        }
    };

    Ok(SandboxBuildResult {
        sandbox,
        worktree_created,
    })
}

async fn mint_github_token(
    creds: &fabro_github::GitHubAppCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String, FabroError> {
    let https_url = fabro_github::ssh_url_to_https(origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&https_url)
        .map_err(|e| FabroError::engine(e.to_string()))?;
    let jwt = fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem)
        .map_err(|e| FabroError::engine(e.to_string()))?;
    let client = reqwest::Client::new();
    let perms_json =
        serde_json::to_value(permissions).map_err(|e| FabroError::engine(e.to_string()))?;
    fabro_github::create_installation_access_token_with_permissions(
        &client,
        &jwt,
        &owner,
        &repo,
        fabro_github::GITHUB_API_BASE_URL,
        perms_json,
    )
    .await
    .map_err(|e| FabroError::engine(e.to_string()))
}

async fn build_sandbox_env(
    spec: &SandboxEnvSpec,
    github_app: Option<&fabro_github::GitHubAppCredentials>,
    emitter: &crate::event::EventEmitter,
) -> Result<HashMap<String, String>, FabroError> {
    let mut env = spec.devcontainer_env.clone();
    env.extend(spec.toml_env.clone());

    if let Some(permissions) = spec.github_permissions.as_ref() {
        if !permissions.is_empty() {
            if let (Some(creds), Some(origin_url)) = (github_app, spec.origin_url.as_deref()) {
                match mint_github_token(creds, origin_url, permissions).await {
                    Ok(token) => {
                        env.insert("GITHUB_TOKEN".to_string(), token);
                    }
                    Err(e) => emit_run_notice(
                        emitter,
                        RunNoticeLevel::Warn,
                        "github_token_failed",
                        format!("Failed to mint GitHub token: {e}"),
                    ),
                }
            }
        }
    }

    Ok(env)
}

async fn build_registry(
    spec: &LlmSpec,
    interviewer: Arc<dyn fabro_interview::Interviewer>,
    sandbox_env: &HashMap<String, String>,
    emitter: &crate::event::EventEmitter,
) -> Result<(Arc<crate::handler::HandlerRegistry>, Option<Client>, bool), FabroError> {
    let build_dry_run = || Arc::new(default_registry(Arc::clone(&interviewer), || None));

    if spec.dry_run {
        return Ok((build_dry_run(), None, true));
    }

    match Client::from_env().await {
        Ok(client) if client.provider_names().is_empty() => {
            emit_run_notice(
                emitter,
                RunNoticeLevel::Warn,
                "dry_run_no_llm",
                "No LLM providers configured. Running in dry-run mode.",
            );
            Ok((build_dry_run(), None, true))
        }
        Ok(client) => {
            let env = sandbox_env.clone();
            let model = spec.model.clone();
            let provider = spec.provider;
            let fallback_chain = spec.fallback_chain.clone();
            let mcp_servers = spec.mcp_servers.clone();
            let registry = Arc::new(default_registry(interviewer, move || {
                let api = AgentApiBackend::new(model.clone(), provider, fallback_chain.clone())
                    .with_env(env.clone())
                    .with_mcp_servers(mcp_servers.clone());
                let cli = AgentCliBackend::new(model.clone(), provider).with_env(env.clone());
                Some(Box::new(BackendRouter::new(Box::new(api), cli)))
            }));
            Ok((registry, Some(client), false))
        }
        Err(e) => {
            emit_run_notice(
                emitter,
                RunNoticeLevel::Warn,
                "dry_run_llm_init_failed",
                format!("Failed to initialize LLM client: {e}. Running in dry-run mode."),
            );
            Ok((build_dry_run(), None, true))
        }
    }
}

async fn resolve_devcontainer(options: &mut InitOptions) -> Result<(), FabroError> {
    let Some(devcontainer) = options.devcontainer.clone() else {
        return Ok(());
    };
    if !devcontainer.enabled {
        return Ok(());
    }

    let config = fabro_devcontainer::DevcontainerResolver::resolve(&devcontainer.resolve_dir)
        .await
        .map_err(|e| FabroError::engine(format!("Failed to resolve devcontainer: {e}")))?;

    let lifecycle_command_count = config.on_create_commands.len()
        + config.post_create_commands.len()
        + config.post_start_commands.len();
    options
        .emitter
        .emit(&WorkflowRunEvent::DevcontainerResolved {
            dockerfile_lines: config.dockerfile.lines().count(),
            environment_count: config.environment.len(),
            lifecycle_command_count,
            workspace_folder: config.workspace_folder.clone(),
        });

    if let SandboxSpec::Daytona {
        config: daytona, ..
    } = &mut options.sandbox
    {
        daytona.snapshot = Some(crate::devcontainer_bridge::devcontainer_to_snapshot_config(
            &config,
        ));
    }

    let timeout = std::time::Duration::from_millis(300_000);
    for command in &config.initialize_commands {
        let shell_commands = match command {
            fabro_devcontainer::Command::Shell(shell) => vec![shell.clone()],
            fabro_devcontainer::Command::Args(args) => {
                vec![args
                    .iter()
                    .map(|arg| try_quote(arg).unwrap_or_else(|_| arg.into()).to_string())
                    .collect::<Vec<_>>()
                    .join(" ")]
            }
            fabro_devcontainer::Command::Parallel(commands) => commands.values().cloned().collect(),
        };

        for shell_command in shell_commands {
            let output = tokio::time::timeout(
                timeout,
                tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&shell_command)
                    .current_dir(&devcontainer.resolve_dir)
                    .output(),
            )
            .await
            .map_err(|_| {
                FabroError::engine(format!(
                    "Devcontainer initializeCommand timed out: {shell_command}"
                ))
            })?
            .map_err(|e| {
                FabroError::engine(format!(
                    "Failed to execute devcontainer initializeCommand: {shell_command}: {e}"
                ))
            })?;

            if !output.status.success() {
                let code = output
                    .status
                    .code()
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string());
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(FabroError::engine(format!(
                    "Devcontainer initializeCommand failed (exit code {code}): {shell_command}\n{stderr}"
                )));
            }
        }
    }

    options.sandbox_env.devcontainer_env = config.environment.clone();
    options.lifecycle.devcontainer_phases = vec![
        ("on_create".to_string(), config.on_create_commands.clone()),
        (
            "post_create".to_string(),
            config.post_create_commands.clone(),
        ),
        ("post_start".to_string(), config.post_start_commands.clone()),
    ];

    Ok(())
}

fn write_sandbox_record(
    run_dir: &Path,
    spec: &SandboxSpec,
    sandbox: &Arc<dyn Sandbox>,
) -> Result<(), anyhow::Error> {
    let working_directory = sandbox.working_directory().to_string();
    let identifier = {
        let info = sandbox.sandbox_info();
        if info.is_empty() {
            None
        } else {
            Some(info)
        }
    };

    let record = match spec {
        SandboxSpec::Docker { config } => SandboxRecord {
            provider: sandbox_provider_name(spec).to_string(),
            working_directory: working_directory.clone(),
            identifier,
            host_working_directory: Some(config.host_working_directory.clone()),
            container_mount_point: Some(working_directory),
            data_host: None,
        },
        SandboxSpec::Ssh { config, .. } => SandboxRecord {
            provider: sandbox_provider_name(spec).to_string(),
            working_directory,
            identifier,
            host_working_directory: None,
            container_mount_point: None,
            data_host: Some(config.destination.clone()),
        },
        _ => SandboxRecord {
            provider: sandbox_provider_name(spec).to_string(),
            working_directory,
            identifier,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        },
    };

    record.save(&run_dir.join("sandbox.json"))
}

/// INITIALIZE phase: prepare the sandbox, env, and handlers for execution.
pub async fn initialize(
    persisted: Persisted,
    mut options: InitOptions,
) -> Result<Initialized, FabroError> {
    let (graph, source, _diagnostics, run_dir, _run_record) = persisted.into_parts();
    options.run_options.run_dir = run_dir.clone();
    options.run_options.git = options.git.clone();

    let hook_runner = if options.hooks.hooks.is_empty() {
        None
    } else {
        Some(Arc::new(HookRunner::new(options.hooks.clone())))
    };

    resolve_devcontainer(&mut options).await?;

    let worktree_plan = resolve_worktree_plan(&mut options).await?;
    if let Some(plan) = worktree_plan.as_ref() {
        options.run_options.git = Some(GitCheckpointOptions {
            base_sha: Some(plan.base_sha.clone()),
            run_branch: Some(plan.branch_name.clone()),
            meta_branch: Some(crate::git::MetadataStore::branch_name(&options.run_id)),
        });
    }

    let sandbox_result = build_sandbox(
        &options.sandbox,
        worktree_plan.as_ref(),
        Arc::clone(&options.emitter),
    )
    .await?;
    if worktree_plan.is_some() && !sandbox_result.worktree_created {
        options.run_options.git = None;
    }

    let sandbox = sandbox_result.sandbox;
    let cleanup_guard = scopeguard::guard(Arc::clone(&sandbox), |sandbox| {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = sandbox.cleanup().await;
            });
        }
    });

    sandbox
        .initialize()
        .await
        .map_err(|e| FabroError::engine(format!("Failed to initialize sandbox: {e}")))?;

    let hook_ctx = HookContext::new(
        HookEvent::SandboxReady,
        options.run_options.run_id.clone(),
        graph.name.clone(),
    );
    let decision = run_hooks(
        hook_runner.as_deref(),
        &hook_ctx,
        Arc::clone(&sandbox),
        None,
    )
    .await;
    if let HookDecision::Block { reason } = decision {
        let msg = reason.unwrap_or_else(|| "blocked by SandboxReady hook".into());
        return Err(FabroError::engine(msg));
    }

    options.emitter.emit(&WorkflowRunEvent::SandboxInitialized {
        working_directory: sandbox.working_directory().to_string(),
    });
    if let Err(e) = write_sandbox_record(&run_dir, &options.sandbox, &sandbox) {
        tracing::warn!(error = %e, "Failed to save sandbox record");
    }

    let env = build_sandbox_env(
        &options.sandbox_env,
        options.run_options.github_app.as_ref(),
        &options.emitter,
    )
    .await?;
    let (registry, llm_client, effective_dry_run) =
        if let Some(registry) = options.registry_override.clone() {
            // A caller-supplied registry owns execution behavior for its handlers.
            (registry, None, options.dry_run)
        } else {
            build_registry(
                &options.llm,
                Arc::clone(&options.interviewer),
                &env,
                &options.emitter,
            )
            .await?
        };
    if effective_dry_run {
        options.dry_run = true;
        options.run_options.settings.dry_run = Some(true);
    }

    let has_run_branch = options
        .run_options
        .git
        .as_ref()
        .and_then(|g| g.run_branch.as_ref())
        .is_some();
    if !has_run_branch {
        match sandbox.setup_git_for_run(&options.run_options.run_id).await {
            Ok(Some(info)) => {
                let base_sha = options
                    .run_options
                    .git
                    .as_ref()
                    .and_then(|g| g.base_sha.clone())
                    .or(Some(info.base_sha.clone()));
                options.run_options.display_base_sha = base_sha.clone();
                options.run_options.git = Some(GitCheckpointOptions {
                    base_sha,
                    run_branch: Some(info.run_branch.clone()),
                    meta_branch: Some(crate::git::MetadataStore::branch_name(
                        &options.run_options.run_id,
                    )),
                });
                if options.run_options.base_branch.is_none() {
                    options.run_options.base_branch = info.base_branch;
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Sandbox git setup failed, running without git checkpoints"
                );
            }
        }
    }

    if !options.lifecycle.setup_commands.is_empty() {
        options.emitter.emit(&WorkflowRunEvent::SetupStarted {
            command_count: options.lifecycle.setup_commands.len(),
        });
        let setup_start = Instant::now();
        for (index, command) in options.lifecycle.setup_commands.iter().enumerate() {
            options
                .emitter
                .emit(&WorkflowRunEvent::SetupCommandStarted {
                    command: command.clone(),
                    index,
                });
            let cmd_start = Instant::now();
            let result = sandbox
                .exec_command(
                    command,
                    options.lifecycle.setup_command_timeout_ms,
                    None,
                    None,
                    None,
                )
                .await
                .map_err(|e| FabroError::engine(format!("Setup command failed: {e}")))?;
            let duration_ms = crate::millis_u64(cmd_start.elapsed());
            if result.exit_code != 0 {
                options.emitter.emit(&WorkflowRunEvent::SetupFailed {
                    command: command.clone(),
                    index,
                    exit_code: result.exit_code,
                    stderr: result.stderr.clone(),
                });
                return Err(FabroError::engine(format!(
                    "Setup command failed (exit code {}): {command}\n{}",
                    result.exit_code, result.stderr,
                )));
            }
            options
                .emitter
                .emit(&WorkflowRunEvent::SetupCommandCompleted {
                    command: command.clone(),
                    index,
                    exit_code: result.exit_code,
                    duration_ms,
                });
        }
        options.emitter.emit(&WorkflowRunEvent::SetupCompleted {
            duration_ms: crate::millis_u64(setup_start.elapsed()),
        });
    }

    for (phase, commands) in &options.lifecycle.devcontainer_phases {
        crate::devcontainer_bridge::run_devcontainer_lifecycle(
            sandbox.as_ref(),
            &options.emitter,
            phase,
            commands,
            options.lifecycle.setup_command_timeout_ms,
        )
        .await
        .map_err(|e| FabroError::engine(e.to_string()))?;
    }

    scopeguard::ScopeGuard::into_inner(cleanup_guard);

    Ok(Initialized {
        graph,
        source,
        run_options: options.run_options,
        checkpoint: options.checkpoint,
        seed_context: options.seed_context,
        emitter: options.emitter,
        sandbox,
        registry,
        hook_runner,
        env,
        dry_run: options.dry_run,
        llm_client,
        model: options.llm.model,
        provider: options.llm.provider,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use chrono::Utc;
    use fabro_config::FabroSettings;
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_interview::AutoApproveInterviewer;

    use super::*;
    use crate::pipeline::types::InitOptions;
    use crate::records::RunRecord;
    use crate::run_options::RunOptions;

    fn simple_graph() -> (Graph, String) {
        let source = r#"digraph test {
  start [shape=Mdiamond];
  exit [shape=Msquare];
  start -> exit;
}"#
        .to_string();
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        graph.nodes.insert("exit".to_string(), exit);
        graph.edges.push(Edge::new("start", "exit"));
        (graph, source)
    }

    fn test_settings(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings: FabroSettings::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            run_id: "run-test".to_string(),
            labels: HashMap::new(),
            git_author: crate::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            host_repo_path: None,
            base_branch: None,
            display_base_sha: None,
            git: None,
        }
    }

    fn test_persisted(graph: Graph, source: String, run_dir: &std::path::Path) -> Persisted {
        Persisted::new(
            graph.clone(),
            source,
            vec![],
            run_dir.to_path_buf(),
            RunRecord {
                run_id: "run-test".to_string(),
                created_at: Utc::now(),
                settings: FabroSettings::default(),
                graph,
                workflow_slug: Some("test".to_string()),
                working_directory: std::env::current_dir().unwrap(),
                host_repo_path: Some(std::env::current_dir().unwrap().display().to_string()),
                base_branch: Some("main".to_string()),
                labels: HashMap::new(),
            },
        )
    }

    #[tokio::test]
    async fn initialize_prepares_sandbox_and_uses_persisted_run_dir() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, source) = simple_graph();
        let persisted = test_persisted(graph, source.clone(), &run_dir);
        let emitter = Arc::new(crate::event::EventEmitter::new());

        let initialized = initialize(
            persisted,
            InitOptions {
                run_id: "run-test".to_string(),
                dry_run: false,
                emitter,
                sandbox: SandboxSpec::Local {
                    working_directory: std::env::current_dir().unwrap(),
                },
                llm: LlmSpec {
                    model: "test-model".to_string(),
                    provider: fabro_llm::Provider::Anthropic,
                    fallback_chain: Vec::new(),
                    mcp_servers: Vec::new(),
                    dry_run: true,
                },
                interviewer: Arc::new(AutoApproveInterviewer),
                lifecycle: crate::run_options::LifecycleOptions {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                run_options: test_settings(&run_dir),
                hooks: fabro_hooks::HookConfig { hooks: vec![] },
                sandbox_env: SandboxEnvSpec {
                    devcontainer_env: HashMap::new(),
                    toml_env: HashMap::from([("TEST_KEY".to_string(), "value".to_string())]),
                    github_permissions: None,
                    origin_url: None,
                },
                devcontainer: None,
                git: None,
                worktree_mode: None,
                registry_override: None,
                checkpoint: None,
                seed_context: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(initialized.run_options.run_dir, run_dir);
        assert_eq!(initialized.source, source);
        assert!(initialized.hook_runner.is_none());
        assert_eq!(
            initialized.env.get("TEST_KEY").map(String::as_str),
            Some("value")
        );
        assert!(initialized.dry_run);
        assert_eq!(initialized.model, "test-model");
        assert_eq!(initialized.provider, fabro_llm::Provider::Anthropic);
        assert!(initialized.llm_client.is_none());
    }

    #[tokio::test]
    async fn initialize_runs_setup_commands() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, source) = simple_graph();
        let persisted = test_persisted(graph, source, &run_dir);
        let emitter = Arc::new(crate::event::EventEmitter::new());

        let initialized = initialize(
            persisted,
            InitOptions {
                run_id: "run-test".to_string(),
                dry_run: false,
                emitter,
                sandbox: SandboxSpec::Local {
                    working_directory: std::env::current_dir().unwrap(),
                },
                llm: LlmSpec {
                    model: "test-model".to_string(),
                    provider: fabro_llm::Provider::Anthropic,
                    fallback_chain: Vec::new(),
                    mcp_servers: Vec::new(),
                    dry_run: true,
                },
                interviewer: Arc::new(AutoApproveInterviewer),
                lifecycle: crate::run_options::LifecycleOptions {
                    setup_commands: vec!["true".to_string()],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                run_options: test_settings(&run_dir),
                hooks: fabro_hooks::HookConfig { hooks: vec![] },
                sandbox_env: SandboxEnvSpec {
                    devcontainer_env: HashMap::new(),
                    toml_env: HashMap::new(),
                    github_permissions: None,
                    origin_url: None,
                },
                devcontainer: None,
                git: None,
                worktree_mode: None,
                registry_override: None,
                checkpoint: None,
                seed_context: None,
            },
        )
        .await
        .unwrap();

        assert!(run_dir.join("sandbox.json").exists());
        assert_eq!(initialized.run_options.run_dir, run_dir);
    }
}
