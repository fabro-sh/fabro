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
use fabro_sandbox::config::WorktreeMode;
use fabro_sandbox::{
    GitSetupIntent, ReadBeforeWriteSandbox, SandboxEventCallback, SandboxSpec, WorktreeOptions,
    WorktreeSandbox,
};
use fabro_vault::Vault;
use futures::future::try_join_all;
use shlex::try_quote;
use tokio::process::Command as TokioCommand;
use tokio::runtime::Handle;
use tokio::sync::RwLock as AsyncRwLock;
use tokio::time::timeout as tokio_timeout;

use super::types::{InitOptions, Initialized, LlmSpec, Persisted, SandboxEnvSpec};
use crate::devcontainer_bridge::{devcontainer_to_snapshot_config, run_devcontainer_lifecycle};
use crate::error::Error;
use crate::event::{Emitter, Event, RunNoticeLevel};
use crate::git::RUN_BRANCH_PREFIX;
use crate::handler::llm::{AgentApiBackend, AgentCliBackend, BackendRouter};
use crate::handler::{HandlerRegistry, default_registry, sandbox_cancel_token};
use crate::run_metadata::{
    RunMetadataRuntime, build_metadata_writer, metadata_branch_name, mint_token,
};
use crate::run_options::{GitCheckpointOptions, RunOptions};
use crate::sandbox_git::GIT_REMOTE;
use crate::sandbox_git_runtime::SandboxGitRuntime;
use crate::services::{EngineServices, RunServices};

struct WorktreePlan {
    branch_name:          String,
    base_sha:             Option<String>,
    worktree_path:        PathBuf,
    skip_branch_creation: bool,
}

async fn resolve_worktree_base_sha(
    sandbox: &dyn Sandbox,
    plan: &WorktreePlan,
) -> Result<Option<String>, Error> {
    if let Some(base_sha) = plan.base_sha.as_ref() {
        return Ok(Some(base_sha.clone()));
    }

    let result = sandbox
        .exec_command(
            &format!("{GIT_REMOTE} rev-parse HEAD"),
            10_000,
            None,
            None,
            None,
        )
        .await
        .map_err(|err| Error::engine_with_source("git rev-parse HEAD failed", &err))?;
    if !result.is_success() {
        let output = result.stderr.trim();
        let output = if output.is_empty() {
            result.stdout.trim()
        } else {
            output
        };
        if is_not_git_repository(output) {
            return Ok(None);
        }
        return Err(Error::engine(format!(
            "git rev-parse HEAD failed (exit {}): {}",
            result.display_exit_code(),
            output
        )));
    }

    let base_sha = result.stdout.trim();
    if base_sha.is_empty() {
        return Err(Error::engine("git rev-parse HEAD returned no commit sha"));
    }
    Ok(Some(base_sha.to_string()))
}

fn is_not_git_repository(output: &str) -> bool {
    output.contains("not a git repository") || output.contains("ambiguous argument 'HEAD'")
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

fn resolve_worktree_plan(options: &mut InitOptions) -> Option<WorktreePlan> {
    let Some(worktree_mode) = options.worktree_mode else {
        options.run_options.display_base_sha = None;
        return None;
    };

    let is_local = matches!(options.sandbox, SandboxSpec::Local { .. });

    if options.checkpoint.is_some() && is_local {
        if let Some(fork_source) = options.run_options.fork_source_ref.as_ref() {
            let base_sha = fork_source.checkpoint_sha.clone();
            options.run_options.display_base_sha = Some(base_sha.clone());
            return Some(WorktreePlan {
                branch_name:          format!("{RUN_BRANCH_PREFIX}{}", options.run_id),
                base_sha:             Some(base_sha),
                worktree_path:        RunScratch::new(&options.run_options.run_dir).worktree_dir(),
                skip_branch_creation: false,
            });
        }

        if let Some(git) = options.run_options.git.as_ref() {
            if let (Some(run_branch), Some(base_sha)) = (&git.run_branch, &git.base_sha) {
                options.run_options.display_base_sha = Some(base_sha.clone());
                return Some(WorktreePlan {
                    branch_name:          run_branch.clone(),
                    base_sha:             Some(base_sha.clone()),
                    worktree_path:        RunScratch::new(&options.run_options.run_dir)
                        .worktree_dir(),
                    skip_branch_creation: true,
                });
            }
        }
    }

    let local_dirty = options
        .run_options
        .pre_run_git
        .as_ref()
        .map(|git| git.dirty);

    if matches!(local_dirty, Some(fabro_types::DirtyStatus::Dirty)) {
        let env_name = if !is_local {
            Some("remote sandbox")
        } else if worktree_mode == WorktreeMode::Never {
            None
        } else {
            Some("worktree")
        };
        if let Some(env_name) = env_name {
            options.emitter.notice(
                RunNoticeLevel::Warn,
                "dirty_worktree",
                format!("Uncommitted changes will not be included in the {env_name}."),
            );
        }
    }

    if !is_local {
        options.run_options.display_base_sha = options
            .run_options
            .pre_run_git
            .as_ref()
            .and_then(|git| git.sha.clone());
        return None;
    }

    if worktree_mode == WorktreeMode::Never {
        options.run_options.display_base_sha = None;
        return None;
    }

    let (branch_name, base_sha) =
        if let Some(fork_source) = options.run_options.fork_source_ref.as_ref() {
            (
                format!("{RUN_BRANCH_PREFIX}{}", options.run_id),
                Some(fork_source.checkpoint_sha.clone()),
            )
        } else {
            (
                format!("{RUN_BRANCH_PREFIX}{}", options.run_id),
                options
                    .run_options
                    .pre_run_git
                    .as_ref()
                    .and_then(|git| git.sha.clone()),
            )
        };
    options.run_options.display_base_sha.clone_from(&base_sha);
    Some(WorktreePlan {
        branch_name,
        base_sha,
        worktree_path: RunScratch::new(&options.run_options.run_dir).worktree_dir(),
        skip_branch_creation: false,
    })
}

fn git_setup_intent(run_options: &RunOptions) -> GitSetupIntent {
    if let Some(source) = run_options.fork_source_ref.as_ref() {
        GitSetupIntent::ForkFromCheckpoint {
            new_run_id:     run_options.run_id.to_string(),
            source_run_id:  source.source_run_id.to_string(),
            checkpoint_sha: source.checkpoint_sha.clone(),
        }
    } else {
        GitSetupIntent::NewRun {
            run_id: run_options.run_id.to_string(),
        }
    }
}

async fn mint_github_token(
    creds: &fabro_github::GitHubCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String, Error> {
    mint_token(creds, origin_url, permissions)
        .await
        .map_err(|err| Error::engine_with_anyhow("Failed to mint GitHub token", &err))
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
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to mint GitHub token");
                        emitter.notice(
                            RunNoticeLevel::Warn,
                            "github_token_failed",
                            format!("Failed to mint GitHub token: {e}"),
                        );
                    }
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
        .map_err(|e| Error::engine_with_source("Failed to resolve devcontainer", &e))?;

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
    let run_shell = |shell_command: String| {
        let cwd = devcontainer.resolve_dir.clone();
        async move {
            let output = tokio_timeout(
                timeout,
                TokioCommand::new("sh")
                    .arg("-c")
                    .arg(&shell_command)
                    .current_dir(&cwd)
                    .output(),
            )
            .await
            .map_err(|_| {
                Error::engine(format!(
                    "Devcontainer initializeCommand timed out: {shell_command}"
                ))
            })?
            .map_err(|e| {
                Error::engine_with_source(
                    format!("Failed to execute devcontainer initializeCommand: {shell_command}"),
                    &e,
                )
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
            Ok::<(), Error>(())
        }
    };

    for command in &config.initialize_commands {
        match command {
            fabro_devcontainer::Command::Shell(shell) => run_shell(shell.clone()).await?,
            fabro_devcontainer::Command::Args(args) => {
                let shell_command = args
                    .iter()
                    .map(|arg| try_quote(arg).unwrap_or_else(|_| arg.into()).to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                run_shell(shell_command).await?;
            }
            fabro_devcontainer::Command::Parallel(commands) => {
                let futures = commands.values().cloned().map(&run_shell);
                try_join_all(futures).await?;
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
    let sandbox_git = Arc::new(SandboxGitRuntime::new());
    let metadata_runtime = Arc::new(RunMetadataRuntime::new());

    let hook_runner = if options.hooks.hooks.is_empty() {
        None
    } else {
        Some(Arc::new(HookRunner::new(
            options.hooks.clone(),
            Arc::clone(&llm_source),
        )))
    };

    resolve_devcontainer(&mut options).await?;

    let worktree_plan = resolve_worktree_plan(&mut options);

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
            .map_err(|e| Error::engine_with_anyhow("Failed to build sandbox", &e))?;
        if let Some(base_sha) = resolve_worktree_base_sha(&*inner, plan).await? {
            sandbox_git
                .ensure_git_available(&*inner)
                .await
                .map_err(|err| Error::engine_with_source("sandbox git unavailable", &err))?;
            options.run_options.display_base_sha = Some(base_sha.clone());
            options.run_options.git = Some(GitCheckpointOptions {
                base_sha:    Some(base_sha.clone()),
                run_branch:  Some(plan.branch_name.clone()),
                meta_branch: Some(metadata_branch_name(&options.run_id.to_string())),
            });
            let mut worktree = WorktreeSandbox::new(inner, WorktreeOptions {
                branch_name: plan.branch_name.clone(),
                base_sha,
                worktree_path: plan.worktree_path.to_string_lossy().into_owned(),
                skip_branch_creation: plan.skip_branch_creation,
                setup_intent: Some(git_setup_intent(&options.run_options)),
            });
            worktree.set_event_callback(Arc::clone(&options.emitter).worktree_callback());
            match worktree.initialize().await {
                Ok(()) => {
                    worktree_created = true;
                    Arc::new(ReadBeforeWriteSandbox::new(Arc::new(worktree)))
                }
                Err(e) => {
                    return Err(Error::engine_with_source("Git worktree setup failed", &e));
                }
            }
        } else {
            tracing::warn!(
                worktree_mode = ?options.worktree_mode,
                "worktree requested but cwd is not a git repository; running without a worktree"
            );
            options.emitter.notice(
                RunNoticeLevel::Warn,
                "worktree_skipped_no_git",
                "Worktree mode requested but no Git repository was found; running without a worktree.",
            );
            Arc::new(ReadBeforeWriteSandbox::new(inner))
        }
    } else {
        Arc::new(ReadBeforeWriteSandbox::new(
            options
                .sandbox
                .build(Some(Arc::clone(&sandbox_event_callback)))
                .await
                .map_err(|e| Error::engine_with_anyhow("Failed to build sandbox", &e))?,
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
        .map_err(|e| Error::engine_with_source("Failed to initialize sandbox", &e))?;

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
        working_directory: sandbox_record.working_directory.clone(),
        provider:          sandbox_record.provider.clone(),
        identifier:        sandbox_record.identifier.clone(),
        repo_cloned:       sandbox_record.repo_cloned,
        clone_origin_url:  sandbox_record.clone_origin_url.clone(),
        clone_branch:      sandbox_record.clone_branch.clone(),
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
        use fabro_types::settings::run::RunMode;

        options.dry_run = true;
        options.run_options.settings.run.execution.mode = RunMode::DryRun;
    }

    let has_run_branch = options
        .run_options
        .git
        .as_ref()
        .and_then(|g| g.run_branch.as_ref())
        .is_some();
    if !has_run_branch {
        let intent = git_setup_intent(&options.run_options);
        if sandbox.origin_url().is_some() {
            sandbox_git
                .ensure_git_available(&*sandbox)
                .await
                .map_err(|err| Error::engine_with_source("sandbox git unavailable", &err))?;
        }
        match sandbox.setup_git(&intent).await {
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
                    meta_branch: Some(metadata_branch_name(
                        &options.run_options.run_id.to_string(),
                    )),
                });
                if options.run_options.base_branch.is_none() {
                    options.run_options.base_branch = info.base_branch;
                }
            }
            Ok(None) => {
                if sandbox.origin_url().is_some() {
                    options.emitter.notice(
                        RunNoticeLevel::Warn,
                        "sandbox_git_unavailable",
                        "Sandbox could not set up Git despite a configured origin; running without checkpointing or PR support.",
                    );
                }
            }
            Err(e) => {
                return Err(Error::engine_with_source("Sandbox git setup failed", &e));
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
                .map_err(|e| Error::engine_with_source("Setup command failed", &e))?;
            if let Some(token) = &cancel_token {
                if token.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                token.cancel();
            }
            let duration_ms = crate::millis_u64(cmd_start.elapsed());
            if !result.is_success() {
                let exit_code = result.display_exit_code();
                let exec_output_tail = result.default_redacted_output_tail();
                options.emitter.emit(&Event::SetupFailed {
                    command: command.clone(),
                    index,
                    exit_code,
                    stderr: result.stderr.clone(),
                    exec_output_tail,
                });
                return Err(Error::engine(format!(
                    "Setup command failed (exit code {}): {command}\n{}",
                    exit_code, result.stderr,
                )));
            }
            let exit_code = result.exit_code.unwrap_or(0);
            options.emitter.emit(&Event::SetupCommandCompleted {
                command: command.clone(),
                index,
                exit_code,
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

    let metadata_writer = match build_metadata_writer(&options.run_options) {
        Ok(writer) => writer,
        Err(err) => {
            let message = format!("failed to initialize checkpoint metadata writer: {err}");
            if metadata_runtime.mark_metadata_degraded() {
                options.emitter.notice(
                    RunNoticeLevel::Warn,
                    "checkpoint_metadata_write_failed",
                    message,
                );
            }
            None
        }
    };

    let run_services = RunServices::new(
        options.run_store.clone(),
        Arc::clone(&options.emitter),
        Arc::clone(&sandbox),
        hook_runner.clone(),
        options.run_options.cancel_token.clone(),
        options.llm.provider,
        Arc::clone(&llm_source),
        sandbox_git,
        metadata_runtime,
        metadata_writer,
    );
    let engine = Arc::new(EngineServices {
        run: Arc::clone(&run_services),
        registry,
        git_state: std::sync::RwLock::new(None),
        env,
        inputs: options.run_options.settings.run.inputs.clone(),
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
    use fabro_sandbox::config::WorktreeMode;
    use fabro_store::Database;
    use fabro_types::{EventBody, RunEvent, RunId, WorkflowSettings, fixtures};
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
            settings:         WorkflowSettings::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     None,
            run_id:           test_run_id(),
            labels:           HashMap::new(),
            workflow_slug:    None,
            github_app:       None,
            pre_run_git:      None,
            fork_source_ref:  None,
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
                settings: WorkflowSettings::default(),
                graph,
                workflow_slug: Some("test".to_string()),
                source_directory: Some(std::env::current_dir().unwrap().display().to_string()),
                git: Some(fabro_types::GitContext {
                    origin_url:   String::new(),
                    branch:       "main".to_string(),
                    sha:          None,
                    dirty:        fabro_types::DirtyStatus::Clean,
                    push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
                }),
                labels: HashMap::new(),
                provenance: None,
                manifest_blob: None,
                definition_blob: None,
                fork_source_ref: None,
                in_place: false,
            },
        )
    }

    async fn initialize_with_setup_command(
        command: &str,
    ) -> (crate::error::Result<Initialized>, Vec<RunEvent>) {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, source) = simple_graph();
        let persisted = test_persisted(graph, source, &run_dir);
        let emitter = Arc::new(crate::event::Emitter::new(test_run_id()));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.clone())
        });

        let result = initialize(persisted, InitOptions {
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
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
            lifecycle: crate::run_options::LifecycleOptions {
                setup_commands:           vec![command.to_string()],
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
        .await;
        let events = seen.lock().unwrap().clone();
        (result, events)
    }

    #[tokio::test]
    async fn resolve_worktree_plan_uses_local_worktree_without_pre_run_git_context() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let store = memory_store();
        let mut options = InitOptions {
            run_id:            test_run_id(),
            run_store:         {
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run:           false,
            emitter:           Arc::new(crate::event::Emitter::new(test_run_id())),
            sandbox:           SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm:               LlmSpec {
                model:          "test-model".to_string(),
                provider:       fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                dry_run:        true,
            },
            interviewer:       Arc::new(AutoApproveInterviewer::engine()),
            lifecycle:         crate::run_options::LifecycleOptions {
                setup_commands:           vec![],
                setup_command_timeout_ms: 1_000,
                devcontainer_phases:      vec![],
            },
            run_options:       test_settings(&run_dir),
            workflow_path:     None,
            workflow_bundle:   None,
            hooks:             fabro_hooks::HookSettings { hooks: vec![] },
            sandbox_env:       SandboxEnvSpec {
                devcontainer_env:   HashMap::new(),
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault:             None,
            devcontainer:      None,
            git:               None,
            worktree_mode:     Some(WorktreeMode::Always),
            run_control:       None,
            registry_override: None,
            artifact_sink:     None,
            checkpoint:        None,
            seed_context:      None,
        };

        let plan = resolve_worktree_plan(&mut options);

        assert!(plan.is_some());
        assert!(options.run_options.display_base_sha.is_none());
        assert!(options.run_options.git.is_none());
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
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
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
            Arc::new(AutoApproveInterviewer::engine()),
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
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
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
    async fn initialize_setup_failure_preserves_stderr_and_adds_exec_tail() {
        let (result, events) =
            initialize_with_setup_command("printf setup-out; printf setup-err >&2; exit 7").await;

        assert!(result.is_err());
        let failed = events
            .iter()
            .find(|event| event.event_name() == "setup.failed")
            .expect("setup failed event");
        match &failed.body {
            EventBody::SetupFailed(props) => {
                assert_eq!(props.exit_code, 7);
                assert_eq!(props.stderr, "setup-err");
                let tail = props.exec_output_tail.as_ref().expect("exec output tail");
                assert_eq!(tail.stdout.as_deref(), Some("setup-out"));
                assert_eq!(tail.stderr.as_deref(), Some("setup-err"));
            }
            other => panic!("expected setup failed body, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn initialize_setup_failure_with_stdout_only_adds_stdout_tail() {
        let (result, events) = initialize_with_setup_command("printf setup-out; exit 5").await;

        assert!(result.is_err());
        let failed = events
            .iter()
            .find(|event| event.event_name() == "setup.failed")
            .expect("setup failed event");
        match &failed.body {
            EventBody::SetupFailed(props) => {
                assert_eq!(props.exit_code, 5);
                assert!(props.stderr.is_empty());
                let tail = props.exec_output_tail.as_ref().expect("exec output tail");
                assert_eq!(tail.stdout.as_deref(), Some("setup-out"));
                assert!(tail.stderr.is_none());
            }
            other => panic!("expected setup failed body, got {other:?}"),
        }
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
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
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
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
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

    #[tokio::test]
    async fn initialize_worktree_skipped_no_git_emits_notice() {
        // Use a non-git temp dir so resolve_worktree_base_sha returns Ok(None)
        let temp = tempfile::tempdir().unwrap();
        let non_git_dir = temp.path().to_path_buf();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, source) = simple_graph();
        let persisted = test_persisted(graph, source, &run_dir);
        let emitter = Arc::new(crate::event::Emitter::new(test_run_id()));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.clone())
        });

        let _initialized = initialize(persisted, InitOptions {
            run_id: test_run_id(),
            run_store: {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run: false,
            emitter,
            sandbox: SandboxSpec::Local {
                working_directory: non_git_dir,
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider:       fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
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
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: None,
            devcontainer: None,
            git: None,
            worktree_mode: Some(WorktreeMode::Always),
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            checkpoint: None,
            seed_context: None,
        })
        .await
        .unwrap();

        let events = seen.lock().unwrap();
        let notice = events.iter().find(|e| {
            matches!(
                &e.body,
                EventBody::RunNotice(props) if props.code == "worktree_skipped_no_git"
            )
        });
        assert!(
            notice.is_some(),
            "expected worktree_skipped_no_git notice, got events: {:?}",
            events.iter().map(RunEvent::event_name).collect::<Vec<_>>()
        );
    }
}
