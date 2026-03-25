use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use fabro_hooks::{HookContext, HookDecision, HookEvent, HookRunner};

use crate::error::FabroError;
use crate::event::WorkflowRunEvent;
use crate::run_settings::GitCheckpointSettings;

use super::types::{InitOptions, Initialized, Persisted};

async fn run_hooks(
    hook_runner: Option<&HookRunner>,
    hook_context: &HookContext,
    sandbox: Arc<dyn fabro_agent::Sandbox>,
    work_dir: Option<&Path>,
) -> HookDecision {
    let Some(runner) = hook_runner else {
        return HookDecision::Proceed;
    };
    runner.run(hook_context, sandbox, work_dir).await
}

/// INITIALIZE phase: prepare the sandbox for execution.
///
/// # Errors
///
/// Returns `FabroError` if sandbox preparation fails.
pub async fn initialize(
    persisted: Persisted,
    mut options: InitOptions,
) -> Result<Initialized, FabroError> {
    let (graph, source, _diagnostics, run_dir, _run_record) = persisted.into_parts();
    options.run_settings.run_dir = run_dir;

    let hook_runner = if options.hooks.hooks.is_empty() {
        None
    } else {
        Some(Arc::new(HookRunner::new(options.hooks)))
    };

    options
        .sandbox
        .initialize()
        .await
        .map_err(|e| FabroError::engine(format!("Failed to initialize sandbox: {e}")))?;

    let hook_ctx = HookContext::new(
        HookEvent::SandboxReady,
        options.run_settings.run_id.clone(),
        graph.name.clone(),
    );
    let decision = run_hooks(
        hook_runner.as_deref(),
        &hook_ctx,
        Arc::clone(&options.sandbox),
        None,
    )
    .await;
    if let HookDecision::Block { reason } = decision {
        let msg = reason.unwrap_or_else(|| "blocked by SandboxReady hook".into());
        return Err(FabroError::engine(msg));
    }

    options.emitter.emit(&WorkflowRunEvent::SandboxInitialized {
        working_directory: options.sandbox.working_directory().to_string(),
    });

    let has_run_branch = options
        .run_settings
        .git
        .as_ref()
        .and_then(|g| g.run_branch.as_ref())
        .is_some();
    if !has_run_branch {
        match options
            .sandbox
            .setup_git_for_run(&options.run_settings.run_id)
            .await
        {
            Ok(Some(info)) => {
                let base_sha = options
                    .run_settings
                    .git
                    .as_ref()
                    .and_then(|g| g.base_sha.clone())
                    .or(Some(info.base_sha));
                options.run_settings.git = Some(GitCheckpointSettings {
                    base_sha,
                    run_branch: Some(info.run_branch.clone()),
                    meta_branch: Some(crate::git::MetadataStore::branch_name(
                        &options.run_settings.run_id,
                    )),
                });
                if options.run_settings.base_branch.is_none() {
                    options.run_settings.base_branch = info.base_branch;
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
        for (index, cmd) in options.lifecycle.setup_commands.iter().enumerate() {
            options
                .emitter
                .emit(&WorkflowRunEvent::SetupCommandStarted {
                    command: cmd.clone(),
                    index,
                });
            let cmd_start = Instant::now();
            let result = options
                .sandbox
                .exec_command(
                    cmd,
                    options.lifecycle.setup_command_timeout_ms,
                    None,
                    None,
                    None,
                )
                .await
                .map_err(|e| FabroError::engine(format!("Setup command failed: {e}")))?;
            let cmd_duration = crate::millis_u64(cmd_start.elapsed());
            if result.exit_code != 0 {
                options.emitter.emit(&WorkflowRunEvent::SetupFailed {
                    command: cmd.clone(),
                    index,
                    exit_code: result.exit_code,
                    stderr: result.stderr.clone(),
                });
                return Err(FabroError::engine(format!(
                    "Setup command failed (exit code {}): {cmd}\n{}",
                    result.exit_code, result.stderr,
                )));
            }
            options
                .emitter
                .emit(&WorkflowRunEvent::SetupCommandCompleted {
                    command: cmd.clone(),
                    index,
                    exit_code: result.exit_code,
                    duration_ms: cmd_duration,
                });
        }
        options.emitter.emit(&WorkflowRunEvent::SetupCompleted {
            duration_ms: crate::millis_u64(setup_start.elapsed()),
        });
    }

    for (phase, commands) in &options.lifecycle.devcontainer_phases {
        crate::devcontainer_bridge::run_devcontainer_lifecycle(
            options.sandbox.as_ref(),
            &options.emitter,
            phase,
            commands,
            options.lifecycle.setup_command_timeout_ms,
        )
        .await
        .map_err(|e| FabroError::engine(e.to_string()))?;
    }

    Ok(Initialized {
        graph,
        source,
        settings: options.run_settings,
        checkpoint: options.checkpoint,
        seed_context: options.seed_context,
        emitter: options.emitter,
        sandbox: options.sandbox,
        registry: options.registry,
        hook_runner,
        env: options.sandbox_env,
        dry_run: options.dry_run,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use chrono::Utc;
    use fabro_config::config::FabroConfig;
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_interview::AutoApproveInterviewer;

    use super::*;
    use crate::handler::default_registry;
    use crate::pipeline::PersistOptions;
    use crate::records::RunRecord;
    use crate::run_settings::RunSettings;

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

    fn test_settings(run_dir: &std::path::Path) -> RunSettings {
        RunSettings {
            config: FabroConfig::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "run-test".to_string(),
            labels: HashMap::new(),
            git_author: crate::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            host_repo_path: None,
            base_branch: None,
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
                config: FabroConfig::default(),
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
        let sandbox = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let registry = Arc::new(default_registry(Arc::new(AutoApproveInterviewer), || None));

        let initialized = initialize(
            persisted,
            InitOptions {
                run_id: "run-test".to_string(),
                dry_run: false,
                emitter,
                sandbox,
                registry,
                lifecycle: crate::run_settings::LifecycleConfig {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                run_settings: test_settings(&run_dir),
                hooks: fabro_hooks::HookConfig { hooks: vec![] },
                sandbox_env: HashMap::from([("TEST_KEY".to_string(), "value".to_string())]),
                checkpoint: None,
                seed_context: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(initialized.settings.run_dir, run_dir);
        assert_eq!(initialized.source, source);
        assert!(initialized.hook_runner.is_none());
        assert_eq!(
            initialized.env.get("TEST_KEY").map(String::as_str),
            Some("value")
        );
    }

    #[tokio::test]
    async fn initialize_skips_empty_graph_source() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, _source) = simple_graph();
        let persisted = test_persisted(graph, String::new(), &run_dir);
        let emitter = Arc::new(crate::event::EventEmitter::new());
        let sandbox = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let registry = Arc::new(default_registry(Arc::new(AutoApproveInterviewer), || None));

        let initialized = initialize(
            persisted,
            InitOptions {
                run_id: "run-test".to_string(),
                dry_run: false,
                emitter,
                sandbox,
                registry,
                lifecycle: crate::run_settings::LifecycleConfig {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                run_settings: test_settings(&run_dir),
                hooks: fabro_hooks::HookConfig { hooks: vec![] },
                sandbox_env: HashMap::new(),
                checkpoint: None,
                seed_context: None,
            },
        )
        .await
        .unwrap();

        assert!(initialized.source.is_empty());
    }

    #[tokio::test]
    async fn initialize_uses_loaded_persisted_run_state() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let wrong_run_dir = temp.path().join("wrong-run-dir");
        let (graph, source) = simple_graph();

        crate::pipeline::persist(
            crate::pipeline::Validated::new(graph.clone(), source.clone(), vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: RunRecord {
                    run_id: "run-test".to_string(),
                    created_at: Utc::now(),
                    config: FabroConfig::default(),
                    graph,
                    workflow_slug: Some("test".to_string()),
                    working_directory: std::env::current_dir().unwrap(),
                    host_repo_path: Some(std::env::current_dir().unwrap().display().to_string()),
                    base_branch: Some("main".to_string()),
                    labels: HashMap::new(),
                },
            },
        )
        .unwrap();

        let loaded = Persisted::load(&run_dir).unwrap();
        let emitter = Arc::new(crate::event::EventEmitter::new());
        let sandbox = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let registry = Arc::new(default_registry(Arc::new(AutoApproveInterviewer), || None));

        let initialized = initialize(
            loaded,
            InitOptions {
                run_id: "run-test".to_string(),
                dry_run: false,
                emitter,
                sandbox,
                registry,
                lifecycle: crate::run_settings::LifecycleConfig {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                run_settings: test_settings(&wrong_run_dir),
                hooks: fabro_hooks::HookConfig { hooks: vec![] },
                sandbox_env: HashMap::new(),
                checkpoint: None,
                seed_context: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(initialized.settings.run_dir, run_dir);
        assert_eq!(initialized.source, source);
    }
}
