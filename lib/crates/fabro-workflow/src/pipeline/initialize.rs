use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fabro_agent::Sandbox;
use fabro_auth::{
    CredentialResolver, CredentialSource, EnvCredentialSource, VaultCredentialSource,
    auth_issue_message,
};
use fabro_config::RunScratch;
use fabro_graphviz::graph;
use fabro_hooks::{HookContext, HookDecision, HookEvent, HookRunner};
use fabro_sandbox::{
    ReadBeforeWriteSandbox, SandboxEventCallback, SandboxSpec, WorkdirStrategy, WorktreeOptions,
    WorktreeSandbox,
};
use fabro_vault::Vault;
use shlex::try_quote;
use tokio::process::Command as TokioCommand;
use tokio::runtime::Handle;
use tokio::sync::RwLock as AsyncRwLock;
use tokio::task::spawn_blocking;
use tokio::time::timeout as tokio_timeout;

use super::types::{InitOptions, Initialized, LlmSpec, Persisted, SandboxEnvSpec};
use crate::devcontainer_bridge::{devcontainer_to_snapshot_config, run_devcontainer_lifecycle};
use crate::error::Error;
use crate::event::{Emitter, Event, RunNoticeLevel};
use crate::git::{self, GitSyncStatus, MetadataStore};
use crate::handler::llm::{AgentApiBackend, AgentCliBackend, BackendRouter};
use crate::handler::{HandlerRegistry, default_registry, sandbox_cancel_token};
use crate::run_options::GitCheckpointOptions;
use crate::services::{EngineServices, RunServices};

struct WorktreePlan {
    branch_name:          String,
    base_sha:             String,
    worktree_path:        PathBuf,
    skip_branch_creation: bool,
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
    emitter: &Emitter,
    level: RunNoticeLevel,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    emitter.emit(&Event::RunNotice {
        level,
        code: code.into(),
        message: message.into(),
    });
}

async fn resolve_worktree_plan(options: &mut InitOptions) -> Result<Option<WorktreePlan>, Error> {
    let Some(worktree_mode) = options.worktree_mode else {
        options.run_options.display_base_sha = None;
        return Ok(None);
    };

    if options.checkpoint.is_some() && matches!(options.sandbox, SandboxSpec::Local { .. }) {
        if let Some(git) = options.run_options.git.as_ref() {
            if let (Some(run_branch), Some(base_sha)) = (&git.run_branch, &git.base_sha) {
                options.run_options.display_base_sha = Some(base_sha.clone());
                return Ok(Some(WorktreePlan {
                    branch_name:          run_branch.clone(),
                    base_sha:             base_sha.clone(),
                    worktree_path:        RunScratch::new(&options.run_options.run_dir)
                        .worktree_dir(),
                    skip_branch_creation: true,
                }));
            }
        }
    }

    let host_repo_path = options
        .run_options
        .host_repo_path
        .clone()
        .or_else(|| options.sandbox.host_repo_path());
    let git_status = if let Some(path) = host_repo_path.as_ref() {
        let path = path.clone();
        let base_branch = options.run_options.base_branch.clone();
        spawn_blocking(move || git::sync_status(&path, "origin", base_branch.as_deref()))
            .await
            .unwrap_or(GitSyncStatus::Dirty)
    } else {
        GitSyncStatus::Dirty
    };
    let strategy = options.sandbox.workdir_strategy(
        worktree_mode,
        git_status.is_clean(),
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
                    spawn_blocking(move || git::branch_needs_push(&repo_path, "origin", &branch))
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
            match spawn_blocking(move || git::head_sha(&repo_path))
                .await
                .unwrap_or_else(|_| Err(Error::engine("git head_sha task panicked")))
            {
                Ok(base_sha) => {
                    options.run_options.display_base_sha = Some(base_sha.clone());
                    Ok(Some(WorktreePlan {
                        branch_name: format!("{}{}", git::RUN_BRANCH_PREFIX, options.run_id),
                        base_sha,
                        worktree_path: RunScratch::new(&options.run_options.run_dir).worktree_dir(),
                        skip_branch_creation: false,
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
            options.run_options.display_base_sha = if let Some(path) = host_repo_path.as_ref() {
                let path = path.clone();
                spawn_blocking(move || git::head_sha(&path))
                    .await
                    .ok()
                    .and_then(std::result::Result::ok)
            } else {
                None
            };
            Ok(None)
        }
        WorkdirStrategy::LocalDirectory => {
            options.run_options.display_base_sha = None;
            Ok(None)
        }
    }
}

async fn mint_github_token(
    creds: &fabro_github::GitHubCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String, Error> {
    if let fabro_github::GitHubCredentials::Token(token) = creds {
        return Ok(token.clone());
    }

    let https_url = fabro_github::ssh_url_to_https(origin_url);
    let (owner, repo) =
        fabro_github::parse_github_owner_repo(&https_url).map_err(|e| Error::engine(e.clone()))?;
    let fabro_github::GitHubCredentials::App(creds) = creds else {
        unreachable!("token credentials return early");
    };
    let jwt = fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem)
        .map_err(|e| Error::engine(e.clone()))?;
    let client = fabro_http::http_client().map_err(|e| Error::engine(e.to_string()))?;
    let perms_json = serde_json::to_value(permissions).map_err(|e| Error::engine(e.to_string()))?;
    fabro_github::create_installation_access_token_with_permissions(
        &client,
        &jwt,
        &owner,
        &repo,
        &fabro_github::github_api_base_url(),
        perms_json,
    )
    .await
    .map_err(|e| Error::engine(e.clone()))
}

async fn build_sandbox_env(
    spec: &SandboxEnvSpec,
    github_app: Option<&fabro_github::GitHubCredentials>,
    emitter: &Emitter,
) -> Result<HashMap<String, String>, Error> {
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
    graph: &graph::Graph,
    llm_source: Arc<dyn CredentialSource>,
    cli_resolver: Option<CredentialResolver>,
) -> Result<(Arc<HandlerRegistry>, bool), Error> {
    let build_no_backend = || Arc::new(default_registry(Arc::clone(&interviewer), || None));

    if spec.dry_run {
        return Ok((build_no_backend(), true));
    }

    let graph_needs_llm = graph
        .nodes
        .values()
        .any(|n| graph::is_llm_handler_type(n.handler_type()));

    match llm_source.resolve().await {
        Ok(result) if result.credentials.is_empty() => {
            if graph_needs_llm {
                let detail = (!result.auth_issues.is_empty()).then(|| {
                    result
                        .auth_issues
                        .iter()
                        .map(|(provider, issue)| auth_issue_message(*provider, issue))
                        .collect::<Vec<_>>()
                        .join("; ")
                });
                let prefix = detail.map_or_else(
                    || "No LLM providers configured".to_string(),
                    |detail| format!("No usable LLM providers configured: {detail}"),
                );
                return Err(Error::Precondition(format!(
                    "{prefix}. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, or pass --dry-run to simulate."
                )));
            }
            Ok((build_no_backend(), false))
        }
        Ok(_result) => {
            let env = sandbox_env.clone();
            let model = spec.model.clone();
            let provider = spec.provider;
            let fallback_chain = spec.fallback_chain.clone();
            let mcp_servers = spec.mcp_servers.clone();
            let llm_source_for_api = Arc::clone(&llm_source);
            let registry = Arc::new(default_registry(interviewer, move || {
                let api = AgentApiBackend::new(
                    model.clone(),
                    provider,
                    fallback_chain.clone(),
                    Arc::clone(&llm_source_for_api),
                )
                .with_env(env.clone())
                .with_mcp_servers(mcp_servers.clone());
                let cli = cli_resolver
                    .clone()
                    .map_or_else(
                        || AgentCliBackend::new_from_env(model.clone(), provider),
                        |resolver| AgentCliBackend::new(model.clone(), provider, resolver),
                    )
                    .with_env(env.clone());
                Some(Box::new(BackendRouter::new(Box::new(api), cli)))
            }));
            Ok((registry, false))
        }
        Err(e) => {
            if graph_needs_llm {
                return Err(Error::Precondition(format!(
                    "Failed to initialize LLM client: {e}. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, or pass --dry-run to simulate.",
                )));
            }
            Ok((build_no_backend(), false))
        }
    }
}

fn build_llm_source(vault: Option<Arc<AsyncRwLock<Vault>>>) -> Arc<dyn CredentialSource> {
    match vault {
        Some(vault) => Arc::new(VaultCredentialSource::new(vault)),
        None => Arc::new(EnvCredentialSource::new()),
    }
}

async fn resolve_devcontainer(options: &mut InitOptions) -> Result<(), Error> {
    let Some(devcontainer) = options.devcontainer.clone() else {
        return Ok(());
    };
    if !devcontainer.enabled {
        return Ok(());
    }

    let config = fabro_devcontainer::DevcontainerResolver::resolve(&devcontainer.resolve_dir)
        .await
        .map_err(|e| Error::engine(format!("Failed to resolve devcontainer: {e}")))?;

    let lifecycle_command_count = config.on_create_commands.len()
        + config.post_create_commands.len()
        + config.post_start_commands.len();
    options.emitter.emit(&Event::DevcontainerResolved {
        dockerfile_lines: config.dockerfile.lines().count(),
        environment_count: config.environment.len(),
        lifecycle_command_count,
        workspace_folder: config.workspace_folder.clone(),
    });

    options
        .sandbox
        .apply_devcontainer_snapshot(devcontainer_to_snapshot_config(&config));

    let timeout = std::time::Duration::from_mins(5);
    for command in &config.initialize_commands {
        let shell_commands = match command {
            fabro_devcontainer::Command::Shell(shell) => vec![shell.clone()],
            fabro_devcontainer::Command::Args(args) => {
                vec![
                    args.iter()
                        .map(|arg| try_quote(arg).unwrap_or_else(|_| arg.into()).to_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                ]
            }
            fabro_devcontainer::Command::Parallel(commands) => commands.values().cloned().collect(),
        };

        for shell_command in shell_commands {
            let output = tokio_timeout(
                timeout,
                TokioCommand::new("sh")
                    .arg("-c")
                    .arg(&shell_command)
                    .current_dir(&devcontainer.resolve_dir)
                    .output(),
            )
            .await
            .map_err(|_| {
                Error::engine(format!(
                    "Devcontainer initializeCommand timed out: {shell_command}"
                ))
            })?
            .map_err(|e| {
                Error::engine(format!(
                    "Failed to execute devcontainer initializeCommand: {shell_command}: {e}"
                ))
            })?;

            if !output.status.success() {
                let code = output
                    .status
                    .code()
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string());
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::engine(format!(
                    "Devcontainer initializeCommand failed (exit code {code}): {shell_command}\n{stderr}"
                )));
            }
        }
    }

    options
        .sandbox_env
        .devcontainer_env
        .clone_from(&config.environment);
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
/// INITIALIZE phase: prepare the sandbox, env, and handlers for execution.
pub async fn initialize(
    persisted: Persisted,
    mut options: InitOptions,
) -> Result<Initialized, Error> {
    let (graph, source, _diagnostics, run_dir, _run_spec) = persisted.into_parts();
    options.run_options.run_dir = run_dir.clone();
    options.run_options.git = options.git.clone();

    let llm_source = build_llm_source(options.vault.clone());
    let cli_resolver = options.vault.clone().map(CredentialResolver::new);

    let hook_runner = if options.hooks.hooks.is_empty() {
        None
    } else {
        Some(Arc::new(HookRunner::new(
            options.hooks.clone(),
            Arc::clone(&llm_source),
        )))
    };

    resolve_devcontainer(&mut options).await?;

    let worktree_plan = resolve_worktree_plan(&mut options).await?;
    if let Some(plan) = worktree_plan.as_ref() {
        options.run_options.git = Some(GitCheckpointOptions {
            base_sha:    Some(plan.base_sha.clone()),
            run_branch:  Some(plan.branch_name.clone()),
            meta_branch: Some(MetadataStore::branch_name(&options.run_id.to_string())),
        });
    }

    let sandbox_event_callback: SandboxEventCallback = {
        let emitter = Arc::clone(&options.emitter);
        Arc::new(move |event| {
            emitter.emit(&Event::Sandbox { event });
        })
    };
    let mut worktree_created = false;
    let sandbox: Arc<dyn Sandbox> = if let Some(plan) = worktree_plan.as_ref() {
        let inner = options
            .sandbox
            .build(Some(Arc::clone(&sandbox_event_callback)))
            .await
            .map_err(|e| Error::engine(e.to_string()))?;
        let mut worktree = WorktreeSandbox::new(inner, WorktreeOptions {
            branch_name:          plan.branch_name.clone(),
            base_sha:             plan.base_sha.clone(),
            worktree_path:        plan.worktree_path.to_string_lossy().into_owned(),
            skip_branch_creation: plan.skip_branch_creation,
        });
        worktree.set_event_callback(Arc::clone(&options.emitter).worktree_callback());
        match worktree.initialize().await {
            Ok(()) => {
                worktree_created = true;
                Arc::new(ReadBeforeWriteSandbox::new(Arc::new(worktree)))
            }
            Err(e) => {
                emit_run_notice(
                    &options.emitter,
                    RunNoticeLevel::Warn,
                    "worktree_setup_failed",
                    format!("Git worktree setup failed ({e}), running without worktree."),
                );
                Arc::new(ReadBeforeWriteSandbox::new(
                    options
                        .sandbox
                        .build(Some(Arc::clone(&sandbox_event_callback)))
                        .await
                        .map_err(|e| Error::engine(e.to_string()))?,
                ))
            }
        }
    } else {
        Arc::new(ReadBeforeWriteSandbox::new(
            options
                .sandbox
                .build(Some(Arc::clone(&sandbox_event_callback)))
                .await
                .map_err(|e| Error::engine(e.to_string()))?,
        ))
    };
    if worktree_plan.is_some() && !worktree_created {
        options.run_options.git = None;
    }
    let cleanup_guard = scopeguard::guard(Arc::clone(&sandbox), |sandbox| {
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                let _ = sandbox.cleanup().await;
            });
        }
    });

    sandbox
        .initialize()
        .await
        .map_err(|e| Error::engine(format!("Failed to initialize sandbox: {e}")))?;

    let hook_ctx = HookContext::new(
        HookEvent::SandboxReady,
        options.run_options.run_id,
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
        return Err(Error::engine(msg));
    }

    let sandbox_record = options.sandbox.to_sandbox_record(&*sandbox);
    options.emitter.emit(&Event::SandboxInitialized {
        working_directory:      sandbox_record.working_directory.clone(),
        provider:               sandbox_record.provider.clone(),
        identifier:             sandbox_record.identifier.clone(),
        host_working_directory: sandbox_record.host_working_directory.clone(),
        container_mount_point:  sandbox_record.container_mount_point.clone(),
    });

    let env = build_sandbox_env(
        &options.sandbox_env,
        options.run_options.github_app.as_ref(),
        &options.emitter,
    )
    .await?;
    let (registry, effective_dry_run) = if let Some(registry) = options.registry_override.clone() {
        // A caller-supplied registry owns execution behavior for its handlers.
        (registry, options.dry_run)
    } else {
        build_registry(
            &options.llm,
            Arc::clone(&options.interviewer),
            &env,
            &graph,
            Arc::clone(&llm_source),
            cli_resolver,
        )
        .await?
    };
    if effective_dry_run {
        use fabro_types::settings::run::{RunExecutionLayer, RunLayer, RunMode};

        options.dry_run = true;
        let run = options
            .run_options
            .settings
            .run
            .get_or_insert_with(RunLayer::default);
        let execution = run.execution.get_or_insert_with(RunExecutionLayer::default);
        execution.mode = Some(RunMode::DryRun);
    }

    let has_run_branch = options
        .run_options
        .git
        .as_ref()
        .and_then(|g| g.run_branch.as_ref())
        .is_some();
    if !has_run_branch {
        match sandbox
            .setup_git_for_run(&options.run_options.run_id.to_string())
            .await
        {
            Ok(Some(info)) => {
                let base_sha = options
                    .run_options
                    .git
                    .as_ref()
                    .and_then(|g| g.base_sha.clone())
                    .or(Some(info.base_sha.clone()));
                options.run_options.display_base_sha.clone_from(&base_sha);
                options.run_options.git = Some(GitCheckpointOptions {
                    base_sha,
                    run_branch: Some(info.run_branch.clone()),
                    meta_branch: Some(MetadataStore::branch_name(
                        &options.run_options.run_id.to_string(),
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
        options.emitter.emit(&Event::SetupStarted {
            command_count: options.lifecycle.setup_commands.len(),
        });
        let setup_start = Instant::now();
        for (index, command) in options.lifecycle.setup_commands.iter().enumerate() {
            options.emitter.emit(&Event::SetupCommandStarted {
                command: command.clone(),
                index,
            });
            let cmd_start = Instant::now();
            let cancel_token = sandbox_cancel_token(options.run_options.cancel_token.clone());
            let result = sandbox
                .exec_command(
                    command,
                    options.lifecycle.setup_command_timeout_ms,
                    None,
                    None,
                    cancel_token.clone(),
                )
                .await
                .map_err(|e| Error::engine(format!("Setup command failed: {e}")))?;
            if let Some(token) = &cancel_token {
                if token.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                token.cancel();
            }
            let duration_ms = crate::millis_u64(cmd_start.elapsed());
            if result.exit_code != 0 {
                options.emitter.emit(&Event::SetupFailed {
                    command: command.clone(),
                    index,
                    exit_code: result.exit_code,
                    stderr: result.stderr.clone(),
                });
                return Err(Error::engine(format!(
                    "Setup command failed (exit code {}): {command}\n{}",
                    result.exit_code, result.stderr,
                )));
            }
            options.emitter.emit(&Event::SetupCommandCompleted {
                command: command.clone(),
                index,
                exit_code: result.exit_code,
                duration_ms,
            });
        }
        options.emitter.emit(&Event::SetupCompleted {
            duration_ms: crate::millis_u64(setup_start.elapsed()),
        });
    }

    for (phase, commands) in &options.lifecycle.devcontainer_phases {
        run_devcontainer_lifecycle(
            sandbox.as_ref(),
            &options.emitter,
            phase,
            commands,
            options.lifecycle.setup_command_timeout_ms,
            options.run_options.cancel_token.clone(),
        )
        .await?;
    }

    let run_services = RunServices::new(
        options.run_store.clone(),
        Arc::clone(&options.emitter),
        Arc::clone(&sandbox),
        hook_runner.clone(),
        options.run_options.cancel_token.clone(),
        options.llm.provider,
        Arc::clone(&llm_source),
    );
    let engine = Arc::new(EngineServices {
        run: Arc::clone(&run_services),
        registry,
        git_state: std::sync::RwLock::new(None),
        env,
        inputs: options
            .run_options
            .settings
            .run
            .as_ref()
            .and_then(|run| run.inputs.clone())
            .unwrap_or_default(),
        dry_run: options.dry_run,
        workflow_path: options.workflow_path.clone(),
        workflow_bundle: options.workflow_bundle.clone(),
    });

    scopeguard::ScopeGuard::into_inner(cleanup_guard);

    Ok(Initialized {
        graph,
        source,
        run_options: options.run_options,
        checkpoint: options.checkpoint,
        seed_context: options.seed_context,
        on_node: None,
        artifact_sink: options.artifact_sink,
        run_control: options.run_control,
        engine,
        model: options.llm.model,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use fabro_auth::{AuthCredential, AuthDetails};
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_interview::AutoApproveInterviewer;
    use fabro_sandbox::SandboxSpec;
    use fabro_store::Database;
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{RunId, fixtures};
    use fabro_vault::{SecretType, Vault};
    use object_store::memory::InMemory;
    use tokio::sync::RwLock as AsyncRwLock;

    use super::*;
    use crate::event::StoreProgressLogger;
    use crate::pipeline::types::InitOptions;
    use crate::records::RunSpec;
    use crate::run_options::RunOptions;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    fn simple_graph() -> (Graph, String) {
        let source = r"digraph test {
  start [shape=Mdiamond];
  exit [shape=Msquare];
  start -> exit;
}"
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

    fn llm_graph() -> (Graph, String) {
        let source = r"digraph test {
  start [shape=Mdiamond];
  writer [shape=box];
  exit [shape=Msquare];
  start -> writer;
  writer -> exit;
}"
        .to_string();
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let mut writer = Node::new("writer");
        writer
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        graph.nodes.insert("writer".to_string(), writer);
        graph.nodes.insert("exit".to_string(), exit);
        graph.edges.push(Edge::new("start", "writer"));
        graph.edges.push(Edge::new("writer", "exit"));
        (graph, source)
    }

    fn test_settings(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings:         SettingsLayer::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     None,
            run_id:           test_run_id(),
            labels:           HashMap::new(),
            workflow_slug:    None,
            github_app:       None,
            host_repo_path:   None,
            base_branch:      None,
            display_base_sha: None,
            git:              None,
        }
    }

    fn test_persisted(graph: Graph, source: String, run_dir: &std::path::Path) -> Persisted {
        Persisted::new(
            graph.clone(),
            source,
            vec![],
            run_dir.to_path_buf(),
            RunSpec {
                run_id: test_run_id(),
                settings: SettingsLayer::default(),
                graph,
                workflow_slug: Some("test".to_string()),
                working_directory: std::env::current_dir().unwrap(),
                host_repo_path: Some(std::env::current_dir().unwrap().display().to_string()),
                repo_origin_url: None,
                base_branch: Some("main".to_string()),
                labels: HashMap::new(),
                provenance: None,
                manifest_blob: None,
                definition_blob: None,
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
        let emitter = Arc::new(crate::event::Emitter::new(test_run_id()));

        let initialized = initialize(persisted, InitOptions {
            run_id: test_run_id(),
            run_store: {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run: false,
            emitter,
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider:       fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer),
            lifecycle: crate::run_options::LifecycleOptions {
                setup_commands:           vec![],
                setup_command_timeout_ms: 1_000,
                devcontainer_phases:      vec![],
            },
            run_options: test_settings(&run_dir),
            workflow_path: None,
            workflow_bundle: None,
            hooks: fabro_hooks::HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                devcontainer_env:   HashMap::new(),
                toml_env:           HashMap::from([("TEST_KEY".to_string(), "value".to_string())]),
                github_permissions: None,
                origin_url:         None,
            },
            vault: None,
            devcontainer: None,
            git: None,
            worktree_mode: None,
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            checkpoint: None,
            seed_context: None,
        })
        .await
        .unwrap();

        assert_eq!(initialized.run_options.run_dir, run_dir);
        assert_eq!(initialized.source, source);
        assert!(initialized.engine.run.hook_runner.is_none());
        assert_eq!(
            initialized.engine.env.get("TEST_KEY").map(String::as_str),
            Some("value")
        );
        assert!(initialized.engine.dry_run);
        assert_eq!(initialized.model, "test-model");
        assert_eq!(
            initialized.engine.run.provider,
            fabro_llm::Provider::Anthropic
        );
        assert!(
            initialized
                .engine
                .run
                .llm_source
                .resolve()
                .await
                .unwrap()
                .credentials
                .is_empty()
        );
    }

    #[tokio::test]
    async fn build_registry_accepts_vault_only_llm_provider() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "anthropic",
                &serde_json::to_string(&AuthCredential {
                    provider: fabro_llm::Provider::Anthropic,
                    details:  AuthDetails::ApiKey {
                        key: "anthropic-key".to_string(),
                    },
                })
                .unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        let (graph, _) = llm_graph();
        let vault = Arc::new(AsyncRwLock::new(vault));

        let (_registry, effective_dry_run) = build_registry(
            &LlmSpec {
                model:          "claude-opus-4-6".to_string(),
                provider:       fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                dry_run:        false,
            },
            Arc::new(AutoApproveInterviewer),
            &HashMap::new(),
            &graph,
            Arc::new(VaultCredentialSource::new(Arc::clone(&vault))),
            Some(CredentialResolver::new(vault)),
        )
        .await
        .unwrap();

        assert!(!effective_dry_run);
    }

    #[tokio::test]
    async fn initialize_runs_setup_commands() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, source) = simple_graph();
        let persisted = test_persisted(graph, source, &run_dir);
        let emitter = Arc::new(crate::event::Emitter::new(test_run_id()));
        let store = memory_store();
        let run_store = store.create_run(&test_run_id()).await.unwrap();
        let store_logger = StoreProgressLogger::new(run_store.clone());
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.event_name().to_string())
        });
        store_logger.register(&emitter);

        let initialized = initialize(persisted, InitOptions {
            run_id: test_run_id(),
            run_store: run_store.into(),
            dry_run: false,
            emitter,
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider:       fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer),
            lifecycle: crate::run_options::LifecycleOptions {
                setup_commands:           vec!["true".to_string()],
                setup_command_timeout_ms: 1_000,
                devcontainer_phases:      vec![],
            },
            run_options: test_settings(&run_dir),
            workflow_path: None,
            workflow_bundle: None,
            hooks: fabro_hooks::HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                devcontainer_env:   HashMap::new(),
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: None,
            devcontainer: None,
            git: None,
            worktree_mode: None,
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            checkpoint: None,
            seed_context: None,
        })
        .await
        .unwrap();
        store_logger.flush().await;

        assert_eq!(initialized.run_options.run_dir, run_dir);
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|event| event == "sandbox.initialized")
        );
    }

    #[tokio::test]
    async fn initialize_cancelled_setup_command_returns_cancelled() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, source) = simple_graph();
        let persisted = test_persisted(graph, source, &run_dir);
        let cancel_token = Arc::new(AtomicBool::new(true));
        let mut run_options = test_settings(&run_dir);
        run_options.cancel_token = Some(cancel_token);

        let result = initialize(persisted, InitOptions {
            run_id: test_run_id(),
            run_store: {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run: false,
            emitter: Arc::new(crate::event::Emitter::new(test_run_id())),
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider:       fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer),
            lifecycle: crate::run_options::LifecycleOptions {
                setup_commands:           vec!["sleep 5".to_string()],
                setup_command_timeout_ms: 5_000,
                devcontainer_phases:      vec![],
            },
            run_options,
            workflow_path: None,
            workflow_bundle: None,
            hooks: fabro_hooks::HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                devcontainer_env:   HashMap::new(),
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: None,
            devcontainer: None,
            git: None,
            worktree_mode: None,
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            checkpoint: None,
            seed_context: None,
        })
        .await;

        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[tokio::test]
    async fn initialize_cancelled_devcontainer_phase_returns_cancelled() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, source) = simple_graph();
        let persisted = test_persisted(graph, source, &run_dir);
        let cancel_token = Arc::new(AtomicBool::new(true));
        let mut run_options = test_settings(&run_dir);
        run_options.cancel_token = Some(cancel_token);

        let result = initialize(persisted, InitOptions {
            run_id: test_run_id(),
            run_store: {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run: false,
            emitter: Arc::new(crate::event::Emitter::new(test_run_id())),
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider:       fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer),
            lifecycle: crate::run_options::LifecycleOptions {
                setup_commands:           vec![],
                setup_command_timeout_ms: 5_000,
                devcontainer_phases:      vec![("on_create".to_string(), vec![
                    fabro_devcontainer::Command::Shell("sleep 5".to_string()),
                ])],
            },
            run_options,
            workflow_path: None,
            workflow_bundle: None,
            hooks: fabro_hooks::HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                devcontainer_env:   HashMap::new(),
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: None,
            devcontainer: None,
            git: None,
            worktree_mode: None,
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            checkpoint: None,
            seed_context: None,
        })
        .await;

        assert!(matches!(result, Err(Error::Cancelled)));
    }
}
