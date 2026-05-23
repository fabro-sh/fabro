use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use fabro_agent::Sandbox;
use fabro_auth::{
    CredentialSource, EnvCredentialSource, VaultCredentialSource, auth_issue_message,
};
use fabro_graphviz::graph;
use fabro_hooks::{HookContext, HookDecision, HookEvent, HookExecutionContext, HookRunner};
use fabro_model::Catalog;
use fabro_sandbox::{
    GitSetupIntent, ReadBeforeWriteSandbox, SandboxEventCallback, SandboxSpec,
    reconnect_for_run_with_callback,
};
use fabro_static::EnvVars;
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
use crate::event::{Event, RunNoticeCode, RunNoticeLevel};
use crate::github_token_source::{AppIatMinter, GitHubTokenSource};
use crate::handler::llm::{AgentAcpBackend, AgentApiBackend, BackendRouter, routing};
use crate::handler::{HandlerRegistry, default_registry};
use crate::run_metadata::{RunMetadataRuntime, build_metadata_writer, metadata_branch_name};
use crate::run_options::{GitCheckpointOptions, RunOptions};
use crate::sandbox_git_runtime::SandboxGitRuntime;
use crate::services::{
    EngineServices, FabroRunToolServices, RunLocations, RunServices, WorkflowToolEnvProvider,
};
use crate::steering_hub::SteeringHub;

type BuiltSandboxEnv = (HashMap<String, String>, Option<Arc<GitHubTokenSource>>);

async fn run_hooks(
    hook_runner: Option<&HookRunner>,
    hook_context: &HookContext,
    sandbox: Arc<dyn Sandbox>,
    execution_context: HookExecutionContext,
) -> HookDecision {
    let Some(runner) = hook_runner else {
        return HookDecision::Proceed;
    };
    runner.run(hook_context, sandbox, execution_context).await
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

fn build_sandbox_env(
    spec: &SandboxEnvSpec,
    github_app: Option<&fabro_github::GitHubCredentials>,
) -> Result<BuiltSandboxEnv, Error> {
    let mut env = spec.devcontainer_env.clone();
    env.extend(spec.toml_env.clone());

    let Some(permissions) = spec.github_permissions.as_ref().filter(|p| !p.is_empty()) else {
        return Ok((env, None));
    };
    let Some(creds) = github_app else {
        return Ok((env, None));
    };

    let source = match creds {
        fabro_github::GitHubCredentials::Pat(token) => {
            Some(Arc::new(GitHubTokenSource::pat(token.clone())))
        }
        fabro_github::GitHubCredentials::Installation(token) => {
            Some(Arc::new(GitHubTokenSource::static_iat(token.clone())))
        }
        fabro_github::GitHubCredentials::App(app) => {
            let Some(origin_url) = spec.origin_url.as_deref() else {
                return Ok((env, None));
            };
            let https_url = fabro_github::ssh_url_to_https(origin_url);
            let (owner, repo) = fabro_github::parse_github_owner_repo(&https_url)
                .map_err(|err| Error::engine_with_anyhow("Failed to parse GitHub origin", err))?;
            let permissions = serde_json::to_value(permissions).map_err(|err| {
                Error::engine_with_source("Failed to serialize GitHub permissions", err)
            })?;
            let http = fabro_http::http_client()
                .map_err(|err| Error::engine_with_source("Failed to build HTTP client", err))?;
            let install_url = app.installation_url(&owner);
            let minter = AppIatMinter::new(
                app.clone(),
                http,
                owner,
                repo,
                fabro_github::github_api_base_url(),
                install_url,
                permissions,
            );
            Some(Arc::new(GitHubTokenSource::mintable(Arc::new(minter))))
        }
    };

    Ok((env, source))
}

async fn build_registry(
    spec: &LlmSpec,
    interviewer: Arc<dyn fabro_interview::Interviewer>,
    steering_hub: Arc<SteeringHub>,
    tool_env_provider: Arc<WorkflowToolEnvProvider>,
    github_token_refresh_managed: bool,
    graph: &graph::Graph,
    llm_source: Arc<dyn CredentialSource>,
    catalog: Arc<Catalog>,
    fabro_run_tools: Option<FabroRunToolServices>,
) -> Result<(Arc<HandlerRegistry>, bool), Error> {
    let no_backend_interviewer = Arc::clone(&interviewer);
    let build_no_backend = move || {
        Arc::new(default_registry(
            Arc::clone(&no_backend_interviewer),
            || None,
        ))
    };

    if spec.dry_run {
        return Ok((build_no_backend(), true));
    }

    let graph_needs_llm = graph
        .nodes
        .values()
        .any(|n| graph::is_llm_handler_type(n.handler_type()));

    if !graph_needs_llm {
        return Ok((build_no_backend(), false));
    }

    let build_llm_registry = || {
        let model = spec.model.clone();
        let provider_id = spec.provider_id.clone();
        let fallback_chain = spec.fallback_chain.clone();
        let mcp_servers = spec.mcp_servers.clone();
        let model_controls = spec.model_controls.clone();
        let llm_source_for_api = Arc::clone(&llm_source);
        let catalog_for_api = Arc::clone(&catalog);
        let steering_hub_for_api = Arc::clone(&steering_hub);
        let tool_env_provider_for_backend = Arc::clone(&tool_env_provider);
        let fabro_run_tools_for_api = fabro_run_tools.clone();
        Arc::new(default_registry(interviewer, move || {
            let tool_env_provider = Arc::clone(&tool_env_provider_for_backend);
            let mut api = AgentApiBackend::new_with_catalog(
                model.clone(),
                provider_id.clone(),
                fallback_chain.clone(),
                Arc::clone(&llm_source_for_api),
                Arc::clone(&steering_hub_for_api),
                Arc::clone(&catalog_for_api),
            )
            .with_run_model_controls(model_controls.clone())
            .with_tool_env_provider(tool_env_provider.clone())
            .with_mcp_servers(mcp_servers.clone());
            if let Some(services) = fabro_run_tools_for_api.clone() {
                api = api.with_fabro_run_tools(services);
            }
            let acp = AgentAcpBackend::new()
                .with_tool_env_provider(tool_env_provider.clone(), github_token_refresh_managed)
                .with_steering_hub(Arc::clone(&steering_hub));
            Some(Box::new(BackendRouter::new(Box::new(api), acp)))
        }))
    };

    if !graph_needs_api_backend(graph) {
        return Ok((build_llm_registry(), false));
    }

    match llm_source.resolve(catalog.as_ref()).await {
        Ok(result) if result.credentials.is_empty() => {
            if graph_needs_llm {
                let detail = (!result.auth_issues.is_empty()).then(|| {
                    result
                        .auth_issues
                        .iter()
                        .map(|(provider, issue)| auth_issue_message(provider, issue))
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
        Ok(_result) => Ok((build_llm_registry(), false)),
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

fn graph_needs_api_backend(graph: &graph::Graph) -> bool {
    graph.nodes.values().any(routing::node_needs_api_backend)
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
        .map_err(|e| Error::engine_with_source("Failed to resolve devcontainer", e))?;

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
                    e,
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
    let (graph, source, _diagnostics, run_dir, run_spec) = persisted.into_parts();
    let host_source_dir = run_spec.source_directory.as_deref().map(PathBuf::from);
    options.run_options.run_dir = run_dir.clone();
    options.run_options.git = options.git.clone();

    let llm_source = build_llm_source(options.vault.clone());
    let catalog = Arc::clone(&options.catalog);
    let sandbox_git = Arc::new(SandboxGitRuntime::new());
    let metadata_runtime = Arc::new(RunMetadataRuntime::new());

    let hook_runner = if options.hooks.hooks.is_empty() {
        None
    } else {
        Some(Arc::new(HookRunner::new(
            options.hooks.clone(),
            Arc::clone(&llm_source),
            Arc::clone(&catalog),
        )))
    };

    resolve_devcontainer(&mut options).await?;

    let attach_existing = options.checkpoint.is_some();
    options.run_options.display_base_sha = options
        .run_options
        .pre_run_git
        .as_ref()
        .and_then(|git| git.sha.clone());
    if !attach_existing
        && !matches!(options.sandbox, SandboxSpec::Local { .. })
        && matches!(
            options
                .run_options
                .pre_run_git
                .as_ref()
                .map(|git| git.dirty),
            Some(fabro_types::DirtyStatus::Dirty)
        )
    {
        options.emitter.notice(
            RunNoticeLevel::Warn,
            RunNoticeCode::DirtyWorktree,
            "Uncommitted changes will not be included in the remote sandbox.",
        );
    }

    let sandbox_event_callback: SandboxEventCallback = {
        let emitter = Arc::clone(&options.emitter);
        Arc::new(move |event| {
            emitter.emit(&Event::Sandbox { event });
        })
    };
    let mut sandbox_initialized = true;
    let sandbox: Arc<dyn Sandbox> = if attach_existing {
        let run_state = options
            .run_store
            .state()
            .await
            .map_err(|err| Error::engine(err.to_string()))?;
        let record = run_state.sandbox.ok_or_else(|| {
            Error::Precondition("cannot resume run: run sandbox is missing".to_string())
        })?;
        let daytona_api_key = match &options.vault {
            Some(vault) => vault
                .read()
                .await
                .get(EnvVars::DAYTONA_API_KEY)
                .map(str::to_string),
            None => None,
        };
        let sandbox = reconnect_for_run_with_callback(
            &record,
            daytona_api_key,
            Some(options.run_id),
            Some(Arc::clone(&sandbox_event_callback)),
        )
        .await
        .map_err(|err| Error::engine_with_anyhow("Failed to reconnect sandbox for resume", err))?;
        sandbox_initialized = false;
        Arc::new(ReadBeforeWriteSandbox::new(Arc::from(sandbox)))
    } else {
        Arc::new(ReadBeforeWriteSandbox::new(
            options
                .sandbox
                .build(Some(Arc::clone(&sandbox_event_callback)))
                .await
                .map_err(|e| Error::engine_with_anyhow("Failed to build sandbox", e))?,
        ))
    };
    let cleanup_guard = (!attach_existing).then(|| {
        scopeguard::guard(Arc::clone(&sandbox), |sandbox| {
            if let Ok(handle) = Handle::try_current() {
                handle.spawn(async move {
                    let _ = sandbox.delete().await;
                });
            }
        })
    });

    if attach_existing {
        sandbox
            .start()
            .await
            .map_err(|e| Error::engine_with_source("Failed to start sandbox", e))?;
    } else {
        sandbox
            .initialize()
            .await
            .map_err(|e| Error::engine_with_source("Failed to initialize sandbox", e))?;
    }

    let locations = RunLocations::for_sandbox(host_source_dir, sandbox.as_ref(), run_dir.clone());

    let hook_ctx = HookContext::new(
        HookEvent::SandboxReady,
        options.run_options.run_id,
        graph.name.clone(),
    );
    let decision = run_hooks(
        hook_runner.as_deref(),
        &hook_ctx,
        Arc::clone(&sandbox),
        locations.hook_execution_context(),
    )
    .await;
    if let HookDecision::Block { reason } = decision {
        let msg = reason.unwrap_or_else(|| "blocked by SandboxReady hook".into());
        return Err(Error::engine(msg));
    }

    if sandbox_initialized {
        let run_sandbox = options
            .sandbox
            .to_run_sandbox(&*sandbox, options.run_options.run_id);
        let runtime = run_sandbox
            .runtime
            .as_ref()
            .ok_or_else(|| Error::engine("initialized sandbox missing runtime metadata"))?;
        options.emitter.emit(&Event::SandboxInitialized {
            working_directory: runtime.working_directory.clone(),
            provider:          run_sandbox.provider,
            id:                runtime.id.clone(),
            repo_cloned:       runtime.repo_cloned,
            clone_origin_url:  runtime.clone_origin_url.clone(),
            clone_branch:      runtime.clone_branch.clone(),
            workspace_root:    runtime.workspace_root.clone(),
            repos_root:        runtime.repos_root.clone(),
            primary_repo_path: runtime.primary_repo_path.clone(),
            primary_repo_link: runtime.primary_repo_link.clone(),
        });
    }

    let (base_env, github_token) = build_sandbox_env(
        &options.sandbox_env,
        options.run_options.github_app.as_ref(),
    )?;
    let tool_env_provider = Arc::new(WorkflowToolEnvProvider {
        base_env:     base_env.clone(),
        github_token: github_token.clone(),
    });
    let github_token_refresh_managed = github_token
        .as_deref()
        .is_some_and(GitHubTokenSource::is_refreshable);
    let (registry, effective_dry_run) = if let Some(registry) = options.registry_override.clone() {
        // A caller-supplied registry owns execution behavior for its handlers.
        (registry, options.dry_run)
    } else {
        build_registry(
            &options.llm,
            Arc::clone(&options.interviewer),
            Arc::clone(&options.steering_hub),
            Arc::clone(&tool_env_provider),
            github_token_refresh_managed,
            &graph,
            Arc::clone(&llm_source),
            Arc::clone(&catalog),
            options.fabro_run_tools.clone(),
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
    if options.run_options.settings.run.run_branch.enabled && !has_run_branch {
        let intent = git_setup_intent(&options.run_options);
        let sandbox_has_origin = sandbox.origin_url().is_some();
        if sandbox_has_origin {
            sandbox_git
                .ensure_git_available(&*sandbox)
                .await
                .map_err(|err| Error::engine_with_source("sandbox git unavailable", err))?;
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
                    meta_branch: options
                        .run_options
                        .settings
                        .run
                        .meta_branch
                        .enabled
                        .then(|| metadata_branch_name(&options.run_options.run_id.to_string())),
                });
                if options.run_options.base_branch.is_none() {
                    options.run_options.base_branch = info.base_branch;
                }
            }
            Ok(None) => {
                if sandbox_has_origin {
                    options.emitter.notice(
                        RunNoticeLevel::Warn,
                        RunNoticeCode::SandboxGitUnavailable,
                        "Sandbox could not set up Git despite a configured origin; running \
                         without checkpointing or PR support.",
                    );
                }
            }
            Err(e) => {
                return Err(Error::engine_with_source("Sandbox git setup failed", e));
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
            let cancel_token = options.run_options.cancel_token.child_token();
            let result = sandbox
                .exec_command(
                    command,
                    options.lifecycle.setup_command_timeout_ms,
                    None,
                    None,
                    Some(cancel_token.clone()),
                )
                .await
                .map_err(|e| Error::engine_with_source("Setup command failed", e))?;
            if options.run_options.cancel_token.is_cancelled() {
                return Err(Error::Cancelled);
            }
            cancel_token.cancel();
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
                    RunNoticeCode::CheckpointMetadataWriteFailed,
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
        locations,
        options.run_options.cancel_token.clone(),
        options.llm.provider_id.clone(),
        options.llm.model.clone(),
        Arc::clone(&llm_source),
        catalog,
        sandbox_git,
        metadata_runtime,
        metadata_writer,
    );
    let engine = Arc::new(EngineServices {
        run: Arc::clone(&run_services),
        registry,
        interviewer: Arc::clone(&options.interviewer),
        git_state: std::sync::RwLock::new(None),
        base_env,
        github_token,
        inputs: options.run_options.settings.run.inputs.clone(),
        dry_run: options.dry_run,
        workflow_path: options.workflow_path.clone(),
        workflow_bundle: options.workflow_bundle.clone(),
    });

    if let Some(cleanup_guard) = cleanup_guard {
        scopeguard::ScopeGuard::into_inner(cleanup_guard);
    }

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
    use std::time::Duration;

    use fabro_acp::test_support::fake_acp_agent_script;
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_interview::AutoApproveInterviewer;
    use fabro_sandbox::SandboxSpec;
    use fabro_store::Database;
    use fabro_types::settings::run::RunModelControls;
    use fabro_types::{EventBody, RunEvent, RunId, WorkflowSettings, fixtures};
    use fabro_vault::{SecretType, Vault};
    use object_store::memory::InMemory;
    use tokio::fs::{create_dir_all, write};
    use tokio::sync::RwLock as AsyncRwLock;

    use super::*;
    use crate::context::{Context, keys};
    use crate::event::StoreProgressLogger;
    use crate::pipeline::types::InitOptions;
    use crate::records::RunSpec;
    use crate::run_options::RunOptions;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn test_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_builtin().expect("default catalog should build"))
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
            cancel_token:     tokio_util::sync::CancellationToken::new(),
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
                graph_source: None,
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
            run_id:            test_run_id(),
            run_store:         {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run:           false,
            emitter:           emitter.clone(),
            sandbox:           SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm:               LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer:       Arc::new(AutoApproveInterviewer::engine()),
            steering_hub:      Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog:           test_catalog(),
            lifecycle:         crate::run_options::LifecycleOptions {
                setup_commands:           vec![command.to_string()],
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
            run_control:       None,
            registry_override: None,
            artifact_sink:     None,
            checkpoint:        None,
            seed_context:      None,
            fabro_run_tools:   None,
        })
        .await;
        let events = seen.lock().unwrap().clone();
        (result, events)
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
            run_id:            test_run_id(),
            run_store:         {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run:           false,
            emitter:           emitter.clone(),
            sandbox:           SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm:               LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer:       Arc::new(AutoApproveInterviewer::engine()),
            steering_hub:      Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog:           test_catalog(),
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
                toml_env:           HashMap::from([("TEST_KEY".to_string(), "value".to_string())]),
                github_permissions: None,
                origin_url:         None,
            },
            vault:             None,
            devcontainer:      None,
            git:               None,
            run_control:       None,
            registry_override: None,
            artifact_sink:     None,
            checkpoint:        None,
            seed_context:      None,
            fabro_run_tools:   None,
        })
        .await
        .unwrap();

        assert_eq!(initialized.run_options.run_dir, run_dir);
        assert_eq!(initialized.source, source);
        assert!(initialized.engine.run.hook_runner.is_none());
        assert_eq!(
            initialized.engine.run.locations.host_source_dir.as_deref(),
            Some(std::env::current_dir().unwrap().as_path())
        );
        assert_eq!(
            initialized.engine.run.locations.sandbox_work_dir.as_deref(),
            Some(std::env::current_dir().unwrap().as_path())
        );
        assert_eq!(
            initialized.engine.run.locations.run_scratch_dir.as_path(),
            run_dir.as_path()
        );
        assert_eq!(
            initialized
                .engine
                .base_env
                .get("TEST_KEY")
                .map(String::as_str),
            Some("value")
        );
        assert!(initialized.engine.dry_run);
        assert_eq!(initialized.model, "test-model");
        assert_eq!(
            initialized.engine.run.provider_id,
            fabro_model::ProviderId::anthropic()
        );
        assert!(
            initialized
                .engine
                .run
                .llm_source
                .resolve(&initialized.engine.run.catalog)
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
                "ANTHROPIC_API_KEY",
                "anthropic-key",
                SecretType::Token,
                None,
            )
            .unwrap();
        let (graph, _) = llm_graph();
        let vault = Arc::new(AsyncRwLock::new(vault));

        let test_emitter = Arc::new(crate::event::Emitter::new(test_run_id()));
        let tool_env_provider = Arc::new(WorkflowToolEnvProvider {
            base_env:     HashMap::new(),
            github_token: None,
        });
        let (_registry, effective_dry_run) = build_registry(
            &LlmSpec {
                model:          "claude-opus-4-6".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        false,
            },
            Arc::new(AutoApproveInterviewer::engine()),
            Arc::new(crate::steering_hub::SteeringHub::new(test_emitter)),
            tool_env_provider,
            false,
            &graph,
            Arc::new(VaultCredentialSource::new(Arc::clone(&vault))),
            test_catalog(),
            None,
        )
        .await
        .unwrap();

        assert!(!effective_dry_run);
    }

    #[tokio::test]
    async fn initialize_executes_acp_backend_node_from_registry() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        create_dir_all(&run_dir).await.unwrap();
        let script_path = temp.path().join("fake_acp_agent.py");
        write(&script_path, fake_acp_agent_script()).await.unwrap();

        let source = format!(
            r#"digraph test {{
  start [shape=Mdiamond];
  writer [type="agent", backend="acp", prompt="write hello", acp.command="python3 {}"];
  exit [shape=Msquare];
  start -> writer;
  writer -> exit;
}}"#,
            script_path.display()
        );
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let mut writer = Node::new("writer");
        writer
            .attrs
            .insert("type".to_string(), AttrValue::String("agent".to_string()));
        writer
            .attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        writer.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("write hello".to_string()),
        );
        writer.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                fabro_sandbox::shell_quote(&script_path.to_string_lossy())
            )),
        );
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

        let mut vault = Vault::load(temp.path().join("secrets.json")).unwrap();
        vault
            .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
            .unwrap();
        let vault = Arc::new(AsyncRwLock::new(vault));

        let emitter = Arc::new(crate::event::Emitter::new(test_run_id()));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.event_name().to_string())
        });
        let store = memory_store();
        let run_store = store.create_run(&test_run_id()).await.unwrap();
        let initialized = initialize(test_persisted(graph, source, &run_dir), InitOptions {
            run_id:            test_run_id(),
            run_store:         run_store.into(),
            dry_run:           false,
            emitter:           emitter.clone(),
            sandbox:           SandboxSpec::Local {
                working_directory: temp.path().to_path_buf(),
            },
            llm:               LlmSpec {
                model:          "fake-acp".to_string(),
                provider_id:    fabro_model::ProviderId::openai(),
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        false,
            },
            interviewer:       Arc::new(AutoApproveInterviewer::engine()),
            steering_hub:      Arc::new(crate::steering_hub::SteeringHub::new(emitter)),
            catalog:           test_catalog(),
            lifecycle:         crate::run_options::LifecycleOptions {
                setup_commands:           Vec::new(),
                setup_command_timeout_ms: 1_000,
                devcontainer_phases:      Vec::new(),
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
            vault:             Some(vault),
            devcontainer:      None,
            git:               None,
            run_control:       None,
            registry_override: None,
            artifact_sink:     None,
            checkpoint:        None,
            seed_context:      None,
            fabro_run_tools:   None,
        })
        .await
        .unwrap();

        let node = initialized.graph.nodes.get("writer").unwrap().clone();
        let handler = initialized.engine.registry.resolve(&node);
        let context = Context::new();
        context.set(
            keys::INTERNAL_RUN_ID,
            serde_json::json!(test_run_id().to_string()),
        );
        let outcome = handler
            .execute(
                &node,
                &context,
                &initialized.graph,
                &initialized.run_options.run_dir,
                &initialized.engine,
            )
            .await
            .unwrap();

        assert_eq!(
            outcome.context_updates.get(&keys::response_key("writer")),
            Some(&serde_json::json!("hello from acp"))
        );
        assert!(
            seen.lock()
                .unwrap()
                .contains(&"agent.acp.started".to_string())
        );
        assert!(
            seen.lock()
                .unwrap()
                .contains(&"agent.acp.completed".to_string())
        );
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
            run_id:            test_run_id(),
            run_store:         run_store.into(),
            dry_run:           false,
            emitter:           emitter.clone(),
            sandbox:           SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm:               LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer:       Arc::new(AutoApproveInterviewer::engine()),
            steering_hub:      Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog:           test_catalog(),
            lifecycle:         crate::run_options::LifecycleOptions {
                setup_commands:           vec!["true".to_string()],
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
            run_control:       None,
            registry_override: None,
            artifact_sink:     None,
            checkpoint:        None,
            seed_context:      None,
            fabro_run_tools:   None,
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
        let cancel_token = tokio_util::sync::CancellationToken::new();
        cancel_token.cancel();
        let mut run_options = test_settings(&run_dir);
        run_options.cancel_token = cancel_token;

        let emitter = Arc::new(crate::event::Emitter::new(test_run_id()));
        let result = initialize(persisted, InitOptions {
            run_id: test_run_id(),
            run_store: {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run: false,
            emitter: emitter.clone(),
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
            steering_hub: Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog: test_catalog(),
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
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            checkpoint: None,
            seed_context: None,
            fabro_run_tools: None,
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
        let cancel_token = tokio_util::sync::CancellationToken::new();
        cancel_token.cancel();
        let mut run_options = test_settings(&run_dir);
        run_options.cancel_token = cancel_token;

        let emitter = Arc::new(crate::event::Emitter::new(test_run_id()));
        let result = initialize(persisted, InitOptions {
            run_id: test_run_id(),
            run_store: {
                let store = memory_store();
                let inner = store.create_run(&test_run_id()).await.unwrap();
                inner.into()
            },
            dry_run: false,
            emitter: emitter.clone(),
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallback_chain: Vec::new(),
                mcp_servers:    Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
            steering_hub: Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog: test_catalog(),
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
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            checkpoint: None,
            seed_context: None,
            fabro_run_tools: None,
        })
        .await;

        assert!(matches!(result, Err(Error::Cancelled)));
    }
}
