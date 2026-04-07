use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fabro_agent::Sandbox;
use fabro_config::RunScratch;
use fabro_graphviz::graph;
use fabro_hooks::{HookContext, HookDecision, HookEvent, HookRunner};
use fabro_llm::client::Client;
use fabro_sandbox::{
    ReadBeforeWriteSandbox, SandboxEventCallback, SandboxSpec, WorkdirStrategy, WorktreeOptions,
    WorktreeSandbox,
};
use shlex::try_quote;

use crate::devcontainer_bridge::{devcontainer_to_snapshot_config, run_devcontainer_lifecycle};
use crate::error::FabroError;
use crate::event::{Emitter, Event, RunNoticeLevel};
use crate::git::{self, GitSyncStatus, MetadataStore};
use crate::handler::llm::{AgentApiBackend, AgentCliBackend, BackendRouter};
use crate::handler::{HandlerRegistry, default_registry};
use crate::run_options::GitCheckpointOptions;
use tokio::process::Command as TokioCommand;
use tokio::runtime::Handle;
use tokio::task::spawn_blocking;
use tokio::time::timeout as tokio_timeout;

use super::types::{InitOptions, Initialized, LlmSpec, Persisted, SandboxEnvSpec};

struct WorktreePlan {
    branch_name: String,
    base_sha: String,
    worktree_path: PathBuf,
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

async fn resolve_worktree_plan(
    options: &mut InitOptions,
) -> Result<Option<WorktreePlan>, FabroError> {
    let Some(worktree_mode) = options.worktree_mode else {
        options.run_options.display_base_sha = None;
        return Ok(None);
    };

    if options.checkpoint.is_some() && matches!(options.sandbox, SandboxSpec::Local { .. }) {
        if let Some(git) = options.run_options.git.as_ref() {
            if let (Some(run_branch), Some(base_sha)) = (&git.run_branch, &git.base_sha) {
                options.run_options.display_base_sha = Some(base_sha.clone());
                return Ok(Some(WorktreePlan {
                    branch_name: run_branch.clone(),
                    base_sha: base_sha.clone(),
                    worktree_path: RunScratch::new(&options.run_options.run_dir).worktree_dir(),
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
    let git_status = host_repo_path
        .as_ref()
        .map_or(GitSyncStatus::Dirty, |path| {
            git::sync_status(path, "origin", options.run_options.base_branch.as_deref())
        });
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
            match git::head_sha(&repo_path) {
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

async fn mint_github_token(
    creds: &fabro_github::GitHubAppCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String, FabroError> {
    let https_url = fabro_github::ssh_url_to_https(origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&https_url)
        .map_err(|e| FabroError::engine(e.clone()))?;
    let jwt = fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem)
        .map_err(|e| FabroError::engine(e.clone()))?;
    let client = reqwest::Client::new();
    let perms_json =
        serde_json::to_value(permissions).map_err(|e| FabroError::engine(e.to_string()))?;
    fabro_github::create_installation_access_token_with_permissions(
        &client,
        &jwt,
        &owner,
        &repo,
        &fabro_github::github_api_base_url(),
        perms_json,
    )
    .await
    .map_err(|e| FabroError::engine(e.clone()))
}

async fn build_sandbox_env(
    spec: &SandboxEnvSpec,
    github_app: Option<&fabro_github::GitHubAppCredentials>,
    emitter: &Emitter,
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
    graph: &graph::Graph,
) -> Result<(Arc<HandlerRegistry>, Option<Client>, bool), FabroError> {
    let build_no_backend = || Arc::new(default_registry(Arc::clone(&interviewer), || None));

    if spec.dry_run {
        return Ok((build_no_backend(), None, true));
    }

    let graph_needs_llm = graph
        .nodes
        .values()
        .any(|n| graph::is_llm_handler_type(n.handler_type()));

    match Client::from_env().await {
        Ok(client) if client.provider_names().is_empty() => {
            if graph_needs_llm {
                return Err(FabroError::Precondition(
                    "No LLM providers configured. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, or pass --dry-run to simulate.".to_string(),
                ));
            }
            Ok((build_no_backend(), None, false))
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
            if graph_needs_llm {
                return Err(FabroError::Precondition(format!(
                    "Failed to initialize LLM client: {e}. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, or pass --dry-run to simulate.",
                )));
            }
            Ok((build_no_backend(), None, false))
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
    options.emitter.emit(&Event::DevcontainerResolved {
        dockerfile_lines: config.dockerfile.lines().count(),
        environment_count: config.environment.len(),
        lifecycle_command_count,
        workspace_folder: config.workspace_folder.clone(),
    });

    options
        .sandbox
        .apply_devcontainer_snapshot(devcontainer_to_snapshot_config(&config));

    let timeout = std::time::Duration::from_millis(300_000);
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
            .map_err(|e| FabroError::engine(e.to_string()))?;
        let mut worktree = WorktreeSandbox::new(
            inner,
            WorktreeOptions {
                branch_name: plan.branch_name.clone(),
                base_sha: plan.base_sha.clone(),
                worktree_path: plan.worktree_path.to_string_lossy().into_owned(),
                skip_branch_creation: plan.skip_branch_creation,
            },
        );
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
                        .map_err(|e| FabroError::engine(e.to_string()))?,
                ))
            }
        }
    } else {
        Arc::new(ReadBeforeWriteSandbox::new(
            options
                .sandbox
                .build(Some(Arc::clone(&sandbox_event_callback)))
                .await
                .map_err(|e| FabroError::engine(e.to_string()))?,
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
        .map_err(|e| FabroError::engine(format!("Failed to initialize sandbox: {e}")))?;

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
        return Err(FabroError::engine(msg));
    }

    let sandbox_record = options.sandbox.to_sandbox_record(&*sandbox);
    options.emitter.emit(&Event::SandboxInitialized {
        working_directory: sandbox_record.working_directory.clone(),
        provider: sandbox_record.provider.clone(),
        identifier: sandbox_record.identifier.clone(),
        host_working_directory: sandbox_record.host_working_directory.clone(),
        container_mount_point: sandbox_record.container_mount_point.clone(),
    });

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
            build_registry(&options.llm, Arc::clone(&options.interviewer), &env, &graph).await?
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
                options.emitter.emit(&Event::SetupFailed {
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
        )
        .await
        .map_err(|e| FabroError::engine(e.to_string()))?;
    }

    scopeguard::ScopeGuard::into_inner(cleanup_guard);

    Ok(Initialized {
        graph,
        source,
        run_options: options.run_options,
        workflow_path: options.workflow_path,
        workflow_bundle: options.workflow_bundle,
        run_store: options.run_store,
        checkpoint: options.checkpoint,
        seed_context: options.seed_context,
        emitter: options.emitter,
        sandbox,
        registry,
        on_node: None,
        artifact_uploader: options.artifact_uploader,
        run_control: options.run_control,
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
    use std::time::Duration;

    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_interview::AutoApproveInterviewer;
    use fabro_sandbox::SandboxSpec;
    use fabro_store::Database;
    use fabro_types::{RunId, Settings, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::StoreProgressLogger;
    use crate::pipeline::types::InitOptions;
    use crate::records::RunRecord;
    use crate::run_options::RunOptions;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
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

    fn test_settings(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings: Settings::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            run_id: test_run_id(),
            labels: HashMap::new(),
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
                run_id: test_run_id(),
                settings: Settings::default(),
                graph,
                workflow_slug: Some("test".to_string()),
                working_directory: std::env::current_dir().unwrap(),
                host_repo_path: Some(std::env::current_dir().unwrap().display().to_string()),
                repo_origin_url: None,
                base_branch: Some("main".to_string()),
                labels: HashMap::new(),
                artifact_storage: None,
                provenance: None,
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

        let initialized = initialize(
            persisted,
            InitOptions {
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
                workflow_path: None,
                workflow_bundle: None,
                hooks: fabro_hooks::HookSettings { hooks: vec![] },
                sandbox_env: SandboxEnvSpec {
                    devcontainer_env: HashMap::new(),
                    toml_env: HashMap::from([("TEST_KEY".to_string(), "value".to_string())]),
                    github_permissions: None,
                    origin_url: None,
                },
                devcontainer: None,
                git: None,
                worktree_mode: None,
                run_control: None,
                registry_override: None,
                artifact_uploader: None,
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

        let initialized = initialize(
            persisted,
            InitOptions {
                run_id: test_run_id(),
                run_store: run_store.into(),
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
                workflow_path: None,
                workflow_bundle: None,
                hooks: fabro_hooks::HookSettings { hooks: vec![] },
                sandbox_env: SandboxEnvSpec {
                    devcontainer_env: HashMap::new(),
                    toml_env: HashMap::new(),
                    github_permissions: None,
                    origin_url: None,
                },
                devcontainer: None,
                git: None,
                worktree_mode: None,
                run_control: None,
                registry_override: None,
                artifact_uploader: None,
                checkpoint: None,
                seed_context: None,
            },
        )
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
}
