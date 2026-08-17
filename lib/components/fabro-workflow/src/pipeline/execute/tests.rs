#![allow(
    clippy::absolute_paths,
    clippy::large_futures,
    reason = "These execution tests favor explicit fixtures over pedantic style lints."
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_auth::test_support as auth_test_support;
use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_hooks::HookSettings;
use fabro_interview::AutoApproveInterviewer;
use fabro_sandbox::SandboxSpec;
use fabro_sandbox::test_support::MockSandbox;
use fabro_store::Database;
use fabro_types::settings::run::RunModelControls;
use fabro_types::{
    Principal, RunId, SystemActorKind, WorkflowSettings, fixtures, format_blob_ref, test_support,
};
use object_store::memory::InMemory;

use super::*;
use crate::context::{self, Context};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope, StoreProgressLogger, append_event};
use crate::handler::start::StartHandler;
use crate::handler::{Handler as HandlerTrait, HandlerRegistry};
use crate::interview_runtime::RunInterviewBlocker;
use crate::model_fallback::ModelFallbackPolicy;
use crate::outcome::{Outcome, OutcomeExt, StageOutcome};
use crate::pipeline::initialize;
use crate::pipeline::types::{InitOptions, LlmSpec, Persisted, ResumeState, SandboxEnvSpec};
use crate::records::RunSpec;
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions, SetupCommand};
use crate::test_support::run_graph;

fn local_env() -> Arc<dyn Sandbox> {
    Arc::new(fabro_agent::LocalSandbox::new(
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ))
}

fn simple_graph() -> Graph {
    let mut g = Graph::new("test_pipeline");
    g.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Run tests".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.edges.push(Edge::new("start", "exit"));
    g
}

fn make_registry() -> HandlerRegistry {
    use crate::handler::exit::ExitHandler;

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry
}

fn test_run_id(label: &str) -> RunId {
    match label {
        "git-cp-test" => fixtures::RUN_2,
        _ => fixtures::RUN_1,
    }
}

fn test_catalog() -> Arc<fabro_model::Catalog> {
    Arc::new(
        fabro_model::Catalog::from_builtin_with_overrides(
            &fabro_model::catalog::LlmCatalogSettings::default(),
        )
        .expect("default catalog should build"),
    )
}

fn test_emitter(label: &str) -> Emitter {
    Emitter::new(test_run_id(label))
}

fn test_emitter_arc(label: &str) -> Arc<Emitter> {
    Arc::new(test_emitter(label))
}

fn test_run_options(run_dir: &Path, run_id: &str) -> RunOptions {
    RunOptions {
        run_dir:          run_dir.to_path_buf(),
        cancel_token:     tokio_util::sync::CancellationToken::new(),
        run_id:           test_run_id(run_id),
        settings:         WorkflowSettings::default(),
        git:              None,
        pre_run_git:      None,
        fork_source_ref:  None,
        labels:           HashMap::new(),
        github_app:       None,
        base_branch:      None,
        display_base_sha: None,
        workflow_slug:    None,
    }
}

fn simple_validated_graph() -> (Graph, String) {
    let source =
        "digraph test { start [shape=Mdiamond]; exit [shape=Msquare]; start -> exit; }".to_string();
    let mut graph = Graph::new("test");

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    graph.edges.push(Edge::new("start", "exit"));
    (graph, source)
}

fn persisted_workflow(graph: Graph, source: String, run_dir: &Path, run_id: RunId) -> Persisted {
    Persisted::new(
        graph.clone(),
        source,
        vec![],
        run_dir.to_path_buf(),
        RunSpec {
            run_id,
            settings: WorkflowSettings::default(),
            graph,
            graph_source: None,
            workflow_slug: Some("test".to_string()),
            automation: None,
            source_directory: Some(
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .display()
                    .to_string(),
            ),
            git: Some(fabro_types::GitContext {
                origin_url: String::new(),
                branch:     "main".to_string(),
                sha:        None,
                dirty:      fabro_types::DirtyStatus::Clean,
            }),
            labels: HashMap::new(),
            provenance: test_support::test_run_provenance(),
            manifest_blob: None,
            definition_blob: None,
            fork_source_ref: None,
        },
    )
}

fn test_lifecycle(setup_commands: Vec<&str>) -> LifecycleOptions {
    LifecycleOptions {
        setup_commands:           setup_commands
            .into_iter()
            .map(|command| SetupCommand {
                command: command.to_string(),
                env:     std::collections::HashMap::new(),
            })
            .collect(),
        setup_command_timeout_ms: 300_000,
    }
}

async fn test_run_store(run_id: &RunId) -> fabro_store::RunDatabase {
    let store: Arc<Database> = Arc::new(Database::new(
        Arc::new(InMemory::new()),
        "",
        Duration::from_millis(1),
        None,
    ));
    store.create_run(run_id).await.unwrap()
}

async fn seed_created_and_starting(
    run_store: &fabro_store::RunDatabase,
    run_options: &RunOptions,
    graph: &Graph,
) {
    append_event(run_store, &run_options.run_id, &Event::RunCreated {
        run_id:           run_options.run_id,
        title:            None,
        settings:         serde_json::to_value(&run_options.settings).unwrap(),
        graph:            serde_json::to_value(graph).unwrap(),
        workflow_source:  None,
        labels:           run_options.labels.clone().into_iter().collect(),
        source_directory: Some(std::env::current_dir().unwrap().display().to_string()),
        workflow_slug:    run_options.workflow_slug.clone(),
        automation:       None,
        provenance:       test_support::test_run_provenance(),
        manifest_blob:    None,
        git:              run_options.pre_run_git.clone(),
        fork_source_ref:  run_options.fork_source_ref.clone(),
        retried_from:     None,
        parent_id:        None,
        web_url:          None,
    })
    .await
    .unwrap();
    append_event(run_store, &run_options.run_id, &Event::RunRunnable {
        source: fabro_types::RunRunnableSource::StartRequested,
        actor:  None,
    })
    .await
    .unwrap();
    append_event(run_store, &run_options.run_id, &Event::RunStarting)
        .await
        .unwrap();
}

async fn execute_test_run(run_dir: &Path, graph: Graph, run_id: &str) -> Executed {
    execute_test_run_with_options(test_run_options(run_dir, run_id), graph, None).await
}

async fn execute_test_run_with_options(
    run_options: RunOptions,
    graph: Graph,
    registry_override: Option<Arc<HandlerRegistry>>,
) -> Executed {
    let run_id_value = run_options.run_id;
    let git_options = run_options.git.clone();
    let run_store = test_run_store(&run_id_value).await;
    seed_created_and_starting(&run_store, &run_options, &graph).await;
    let emitter = test_emitter_arc("test-run");
    let store_logger = StoreProgressLogger::new(run_store.clone());
    store_logger.register(&emitter);
    let initialized = initialize(
        persisted_workflow(graph, String::new(), &run_options.run_dir, run_id_value),
        InitOptions {
            run_store: run_store.into(),
            dry_run: false,
            emitter: emitter.clone(),
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallbacks:      ModelFallbackPolicy::default(),
                mcp_servers:    Vec::new(),
                skill_dirs:     Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
            steering_hub: Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog: test_catalog(),
            lifecycle: LifecycleOptions {
                setup_commands:           vec![],
                setup_command_timeout_ms: 1_000,
            },
            run_options,
            workflow_path: None,
            workflow_bundle: None,
            hooks: HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: auth_test_support::empty_vault(),
            git: git_options,
            run_control: None,
            registry_override,
            artifact_sink: None,
            resume: None,
            seed_context: None,
            fabro_run_tools: None,
        },
    )
    .await
    .unwrap();

    let executed = execute(initialized).await;
    store_logger.flush().await;
    executed
}

#[tokio::test]
async fn execute_runs_start_to_exit_and_returns_final_context() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let (graph, source) = simple_validated_graph();
    let run_options = test_run_options(&run_dir, "run-test");
    let run_store = test_run_store(&test_run_id("run-test")).await;
    seed_created_and_starting(&run_store, &run_options, &graph).await;
    let initialized = initialize(
        persisted_workflow(graph, source, &run_dir, test_run_id("run-test")),
        InitOptions {
            run_store: run_store.into(),
            dry_run: false,
            emitter: test_emitter_arc("run-test"),
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallbacks:      ModelFallbackPolicy::default(),
                mcp_servers:    Vec::new(),
                skill_dirs:     Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
            steering_hub: Arc::new(crate::steering_hub::SteeringHub::new(test_emitter_arc(
                "run-test",
            ))),
            catalog: test_catalog(),
            lifecycle: LifecycleOptions {
                setup_commands:           vec![],
                setup_command_timeout_ms: 1_000,
            },
            run_options,
            workflow_path: None,
            workflow_bundle: None,
            hooks: HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: auth_test_support::empty_vault(),
            git: None,
            run_control: None,
            registry_override: None,
            artifact_sink: None,
            resume: None,
            seed_context: None,
            fabro_run_tools: None,
        },
    )
    .await
    .unwrap();

    let executed = execute(initialized).await;

    assert_eq!(
        executed.outcome.as_ref().unwrap().status,
        crate::outcome::StageOutcome::Succeeded
    );
    assert_eq!(
        executed
            .final_context
            .get(crate::context::keys::INTERNAL_RUN_ID),
        Some(serde_json::json!(test_run_id("run-test").to_string()))
    );
}

#[tokio::test]
async fn resumed_in_flight_node_starts_a_new_stage_execution() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();

    // start -> work -> exit; `work` resolves to the default (dry-run) handler.
    let mut graph = Graph::new("resume_identity");
    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);
    graph.nodes.insert("work".to_string(), Node::new("work"));
    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let run_options = test_run_options(&run_dir, "resume-identity");
    let run_id = run_options.run_id;
    let run_store = test_run_store(&run_id).await;
    seed_created_and_starting(&run_store, &run_options, &graph).await;
    // Resume reconnects to the previously recorded sandbox.
    append_event(&run_store, &run_id, &Event::SandboxInitialized {
        working_directory: std::env::current_dir().unwrap().display().to_string(),
        provider:          fabro_types::SandboxProviderKind::Local,
        id:                "local".to_string(),
        image:             None,
        snapshot:          None,
        repo_cloned:       None,
        clone_origin_url:  None,
        clone_branch:      None,
        workspace_root:    None,
        repos_root:        None,
        primary_repo_path: None,
        primary_repo_link: None,
    })
    .await
    .unwrap();
    let emitter = test_emitter_arc("resume-identity");
    let events: Arc<std::sync::Mutex<Vec<fabro_types::RunEvent>>> = Arc::default();
    {
        let events = Arc::clone(&events);
        emitter.on_event(move |event| {
            events
                .lock()
                .expect("event capture mutex should not be poisoned")
                .push(event.clone());
        });
    }

    // Simulate resuming after `work@1` was cancelled mid-flight: the selected
    // checkpoint predates `work`, while the allocator seed carries the
    // projection-observed high-water mark and provenance link.
    let checkpoint = crate::records::Checkpoint {
        timestamp:                  chrono::Utc::now(),
        current_node:               "start".to_string(),
        completed_nodes:            vec!["start".to_string()],
        node_retries:               HashMap::new(),
        context_values:             HashMap::new(),
        node_outcomes:              HashMap::new(),
        next_node_id:               Some("work".to_string()),
        git_commit_sha:             None,
        loop_failure_signatures:    HashMap::new(),
        restart_failure_signatures: HashMap::new(),
        node_visits:                HashMap::from([("start".to_string(), 1usize)]),
    };
    let seed = crate::stage_execution::StageExecutionSeed::test_with_high_water(
        &fabro_types::StageId::new("work", 1),
        Some(fabro_types::StageId::new("work", 1)),
    );
    let resume = ResumeState::for_test(checkpoint, seed);

    let initialized = initialize(
        persisted_workflow(graph, String::new(), &run_dir, run_id),
        InitOptions {
            run_store: run_store.into(),
            dry_run: false,
            emitter: emitter.clone(),
            sandbox: SandboxSpec::Local {
                working_directory: std::env::current_dir().unwrap(),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallbacks:      ModelFallbackPolicy::default(),
                mcp_servers:    Vec::new(),
                skill_dirs:     Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
            steering_hub: Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog: test_catalog(),
            lifecycle: LifecycleOptions {
                setup_commands:           vec![],
                setup_command_timeout_ms: 1_000,
            },
            run_options,
            workflow_path: None,
            workflow_bundle: None,
            hooks: HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: auth_test_support::empty_vault(),
            git: None,
            run_control: None,
            registry_override: Some(Arc::new(make_registry())),
            artifact_sink: None,
            resume: Some(resume),
            seed_context: None,
            fabro_run_tools: None,
        },
    )
    .await
    .unwrap();

    let executed = execute(initialized).await;
    assert_eq!(executed.outcome.unwrap().status, StageOutcome::Succeeded);

    let events = events
        .lock()
        .expect("event capture mutex should not be poisoned");
    let work_started = events
        .iter()
        .find(|event| {
            matches!(event.body, fabro_types::EventBody::StageStarted(_))
                && event.node_id.as_deref() == Some("work")
        })
        .expect("resumed run should emit stage.started for work");
    // The reexecution owns a fresh StageId while the graph visit stays at 1.
    assert_eq!(
        work_started.stage_id,
        Some(fabro_types::StageId::new("work", 2))
    );
    let fabro_types::EventBody::StageStarted(props) = &work_started.body else {
        panic!("expected stage.started body");
    };
    assert_eq!(props.graph_visit, Some(1));
    assert_eq!(
        props.resumed_from_stage_id,
        Some(fabro_types::StageId::new("work", 1))
    );

    // Every later stage-scoped event from this invocation carries the same
    // execution id, including the checkpoint envelope.
    let work_checkpoint = events
        .iter()
        .find(|event| {
            matches!(event.body, fabro_types::EventBody::CheckpointCompleted(_))
                && event.node_id.as_deref() == Some("work")
        })
        .expect("resumed run should checkpoint work");
    assert_eq!(
        work_checkpoint.stage_id,
        Some(fabro_types::StageId::new("work", 2))
    );

    // A node without a prior observable execution starts at ordinal 1.
    let exit_started = events
        .iter()
        .find(|event| {
            matches!(event.body, fabro_types::EventBody::StageStarted(_))
                && event.node_id.as_deref() == Some("exit")
        })
        .expect("terminal node should emit its synthetic stage.started");
    assert_eq!(
        exit_started.stage_id,
        Some(fabro_types::StageId::new("exit", 1))
    );
}

async fn run_with_lifecycle(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &Graph,
    run_options: RunOptions,
    lifecycle: LifecycleOptions,
) -> Result<Outcome, Error> {
    std::fs::create_dir_all(&run_options.run_dir).unwrap();
    let run_dir = run_options.run_dir.clone();
    let run_id = run_options.run_id;
    let run_store = test_run_store(&run_id).await;
    seed_created_and_starting(&run_store, &run_options, graph).await;
    let initialized = initialize(
        persisted_workflow(graph.clone(), String::new(), &run_dir, run_id),
        InitOptions {
            run_store: run_store.into(),
            dry_run: false,
            emitter: emitter.clone(),
            sandbox: SandboxSpec::Local {
                working_directory: PathBuf::from(sandbox.working_directory()),
            },
            llm: LlmSpec {
                model:          "test-model".to_string(),
                provider_id:    fabro_model::ProviderId::anthropic(),
                fallbacks:      ModelFallbackPolicy::default(),
                mcp_servers:    Vec::new(),
                skill_dirs:     Vec::new(),
                model_controls: RunModelControls::default(),
                dry_run:        true,
            },
            interviewer: Arc::new(AutoApproveInterviewer::engine()),
            steering_hub: Arc::new(crate::steering_hub::SteeringHub::new(emitter.clone())),
            catalog: test_catalog(),
            lifecycle,
            run_options,
            workflow_path: None,
            workflow_bundle: None,
            hooks: HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                toml_env:           HashMap::new(),
                github_permissions: None,
                origin_url:         None,
            },
            vault: auth_test_support::empty_vault(),
            git: None,
            run_control: None,
            registry_override: Some(Arc::new(registry)),
            artifact_sink: None,
            resume: None,
            seed_context: None,
            fabro_run_tools: None,
        },
    )
    .await?;
    super::execute(initialized).await.outcome
}

struct AlwaysFailHandler;

#[async_trait]
impl HandlerTrait for AlwaysFailHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &crate::handler::EngineServices,
    ) -> std::result::Result<Outcome, Error> {
        Ok(Outcome::fail_classify("always fails"))
    }
}

struct SlowHandler {
    sleep_ms: u64,
}

#[async_trait]
impl HandlerTrait for SlowHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &crate::handler::EngineServices,
    ) -> std::result::Result<Outcome, Error> {
        tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
        Ok(Outcome::success())
    }
}

struct InterviewWaitHandler {
    wait_ms:   u64,
    active_ms: u64,
}

#[async_trait]
impl HandlerTrait for InterviewWaitHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &crate::handler::EngineServices,
    ) -> std::result::Result<Outcome, Error> {
        let stage_id = StageScope::for_handler(context, &node.id).stage_id();
        let guard = services
            .run
            .interview_blocker
            .block(Arc::clone(&services.run.emitter), stage_id);
        tokio::time::sleep(Duration::from_millis(self.wait_ms)).await;
        guard.resolve();
        tokio::time::sleep(Duration::from_millis(self.active_ms)).await;
        Ok(Outcome::success())
    }
}

fn interview_wait_graph(stall_timeout: Duration, node_timeout: Option<Duration>) -> Graph {
    let mut graph = simple_graph();
    graph.attrs.insert(
        "stall_timeout".to_string(),
        AttrValue::Duration(stall_timeout),
    );
    graph
        .attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));

    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("interview_wait".to_string()),
    );
    if let Some(timeout) = node_timeout {
        work.attrs
            .insert("timeout".to_string(), AttrValue::Duration(timeout));
    }
    graph.nodes.insert("work".to_string(), work);
    graph.edges.clear();
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));
    graph
}

struct StopsSandboxHandler {
    sandbox: Arc<MockSandbox>,
}

#[async_trait]
impl HandlerTrait for StopsSandboxHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &crate::handler::EngineServices,
    ) -> std::result::Result<Outcome, Error> {
        self.sandbox
            .stop()
            .await
            .map_err(|err| Error::handler_with_source("failed to stop test sandbox", err))?;
        Ok(Outcome::success())
    }
}

struct PanickingHandler;

#[async_trait]
impl HandlerTrait for PanickingHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &crate::handler::EngineServices,
    ) -> std::result::Result<Outcome, Error> {
        panic!("test panic message");
    }
}

struct BlobCommandOutputHandler;

#[async_trait]
impl HandlerTrait for BlobCommandOutputHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &crate::handler::EngineServices,
    ) -> std::result::Result<Outcome, Error> {
        let blob = serde_json::to_vec("routed-ok").unwrap();
        let blob_id = services.run.run_store.write_blob(&blob).await.unwrap();
        let mut outcome = Outcome::success();
        outcome.context_updates.insert(
            context::keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!(format_blob_ref(&blob_id)),
        );
        Ok(outcome)
    }
}

struct FailOnceThenSucceedHandler {
    call_count: AtomicU32,
}

#[async_trait]
impl HandlerTrait for FailOnceThenSucceedHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &crate::handler::EngineServices,
    ) -> std::result::Result<Outcome, Error> {
        if self.call_count.fetch_add(1, Ordering::Relaxed) == 0 {
            Err(Error::handler("transient failure"))
        } else {
            Ok(Outcome::success())
        }
    }
}

fn cyclic_graph() -> Graph {
    let mut g = Graph::new("cyclic");
    g.attrs
        .insert("goal".to_string(), AttrValue::String("loop".to_string()));
    g.attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);
    g.nodes.insert("work".to_string(), Node::new("work"));

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.edges.push(Edge::new("start", "work"));
    let mut cond_edge = Edge::new("work", "exit");
    cond_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=never_matches".to_string()),
    );
    g.edges.push(cond_edge);
    g.edges.push(Edge::new("work", "work"));
    g
}

fn looping_fail_graph() -> Graph {
    let mut g = Graph::new("loop_fail");
    g.attrs
        .insert("goal".to_string(), AttrValue::String("test".to_string()));
    g.attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("always_fail".to_string()),
    );
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    g.nodes.insert("work".to_string(), work);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.edges.push(Edge::new("start", "work"));
    let mut fail_edge = Edge::new("work", "work");
    fail_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=failed".to_string()),
    );
    g.edges.push(fail_edge);
    let mut ok_edge = Edge::new("work", "exit");
    ok_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=succeeded".to_string()),
    );
    g.edges.push(ok_edge);
    g
}

#[tokio::test]
async fn execute_runs_simple_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run_graph(
        make_registry(),
        test_emitter_arc("test-run"),
        local_env(),
        &simple_graph(),
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, StageOutcome::Succeeded);
}

#[tokio::test]
async fn execute_preserves_sandbox_activation_error_chain() {
    let dir = tempfile::tempdir().unwrap();
    let sandbox: Arc<dyn Sandbox> =
        Arc::new(MockSandbox::linux().with_activate_error("provider unavailable"));

    let error = run_graph(
        make_registry(),
        test_emitter_arc("test-run"),
        sandbox,
        &simple_graph(),
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .expect_err("sandbox activation should fail");

    assert_eq!(error.causes(), vec![
        "failed to activate sandbox before node start",
        "Mock sandbox activation failed",
        "provider unavailable",
    ]);
}

#[tokio::test]
async fn execute_reactivates_sandbox_after_a_stage_can_leave_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let sandbox = Arc::new(MockSandbox::linux());
    let mut registry = make_registry();
    registry.register(
        "start",
        Box::new(StopsSandboxHandler {
            sandbox: Arc::clone(&sandbox),
        }),
    );
    let sandbox_for_run: Arc<dyn Sandbox> = sandbox.clone();
    let mut run_options = test_run_options(dir.path(), "test-run");
    run_options
        .settings
        .run
        .artifacts
        .include
        .push("**/*".to_string());

    let outcome = run_graph(
        registry,
        test_emitter_arc("test-run"),
        sandbox_for_run,
        &simple_graph(),
        &run_options,
    )
    .await
    .unwrap();

    assert_eq!(outcome.status, StageOutcome::Succeeded);
    assert_eq!(sandbox.stop_count(), 1);
    assert!(sandbox.walk_files_was_called());
    assert!(!sandbox.walked_while_inactive());
}

#[tokio::test]
async fn execute_saves_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let executed = execute_test_run(dir.path(), simple_graph(), "test-run").await;
    assert!(
        executed
            .engine
            .run
            .run_store
            .state()
            .await
            .unwrap()
            .current_checkpoint()
            .is_some()
    );
}

#[tokio::test]
async fn execute_emits_events() {
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let emitter = test_emitter("test-run");
    emitter.on_event(move |event| {
        events_clone.lock().unwrap().push(format!("{event:?}"));
    });

    run_graph(
        make_registry(),
        Arc::new(emitter),
        local_env(),
        &simple_graph(),
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .unwrap();

    assert!(events.lock().unwrap().len() >= 4);
}

#[tokio::test]
async fn execute_error_when_no_start_node() {
    let dir = tempfile::tempdir().unwrap();
    let result = run_graph(
        make_registry(),
        test_emitter_arc("test-run"),
        local_env(),
        &Graph::new("empty"),
        &test_run_options(dir.path(), "test-run"),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn execute_mirrors_graph_goal_to_context() {
    let dir = tempfile::tempdir().unwrap();
    let executed = execute_test_run(dir.path(), simple_graph(), "test-run").await;
    let cp = executed
        .engine
        .run
        .run_store
        .state()
        .await
        .unwrap()
        .current_checkpoint()
        .cloned()
        .unwrap();
    assert_eq!(
        cp.context_values.get(context::keys::GRAPH_GOAL),
        Some(&serde_json::json!("Run tests"))
    );
}

#[tokio::test]
async fn execute_conditional_routing_uses_unconditional_success_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Graph::new("cond_test");

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.nodes.insert("path_a".to_string(), Node::new("path_a"));
    g.nodes.insert("path_b".to_string(), Node::new("path_b"));

    let mut e1 = Edge::new("start", "path_a");
    e1.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=failed".to_string()),
    );
    g.edges.push(e1);
    g.edges.push(Edge::new("start", "path_b"));
    g.edges.push(Edge::new("path_a", "exit"));
    g.edges.push(Edge::new("path_b", "exit"));

    let executed = execute_test_run(dir.path(), g, "test-run").await;
    let cp = executed
        .engine
        .run
        .run_store
        .state()
        .await
        .unwrap()
        .current_checkpoint()
        .cloned()
        .unwrap();
    assert!(cp.completed_nodes.contains(&"path_b".to_string()));
    assert!(!cp.completed_nodes.contains(&"path_a".to_string()));
}

#[tokio::test]
async fn execute_conditional_routing_resolves_command_output_blob_refs() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Graph::new("command_output_route");

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut commandish = Node::new("commandish");
    commandish.attrs.insert(
        "type".to_string(),
        AttrValue::String("blob_command_output".to_string()),
    );
    commandish
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    g.nodes.insert("commandish".to_string(), commandish);

    g.nodes.insert("matched".to_string(), Node::new("matched"));
    g.nodes
        .insert("fallback".to_string(), Node::new("fallback"));

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.edges.push(Edge::new("start", "commandish"));
    let mut matched = Edge::new("commandish", "matched");
    matched.attrs.insert(
        "condition".to_string(),
        AttrValue::String("command.output contains routed-ok".to_string()),
    );
    g.edges.push(matched);
    g.edges.push(Edge::new("commandish", "fallback"));
    g.edges.push(Edge::new("matched", "exit"));
    g.edges.push(Edge::new("fallback", "exit"));

    let mut registry = make_registry();
    registry.register("blob_command_output", Box::new(BlobCommandOutputHandler));
    let executed = execute_test_run_with_options(
        test_run_options(dir.path(), "test-run"),
        g,
        Some(Arc::new(registry)),
    )
    .await;

    let cp = executed
        .engine
        .run
        .run_store
        .state()
        .await
        .unwrap()
        .current_checkpoint()
        .cloned()
        .unwrap();
    assert!(cp.completed_nodes.contains(&"matched".to_string()));
    assert!(!cp.completed_nodes.contains(&"fallback".to_string()));
    assert!(
        cp.context_values[context::keys::COMMAND_OUTPUT]
            .as_str()
            .is_some_and(|value| value.starts_with("blob://sha256/")),
        "durable checkpoint context should keep the command output blob ref"
    );
}

#[tokio::test]
async fn execute_persists_start_record_and_node_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut run_options = test_run_options(dir.path(), "test-run");
    run_options.git = Some(GitCheckpointOptions {
        base_sha:    Some("abc123".into()),
        run_branch:  Some(format!("fabro/run/{}", test_run_id("test-run"))),
        meta_branch: None,
    });

    let executed = execute_test_run_with_options(run_options, simple_graph(), None).await;
    let state = executed.engine.run.run_store.state().await.unwrap();
    let start = state.start.as_ref().unwrap();
    assert_eq!(
        start.run_branch.as_deref(),
        Some(format!("fabro/run/{}", test_run_id("test-run")).as_str())
    );
    assert_eq!(start.base_sha.as_deref(), Some("abc123"));

    let node = state.stage(&fabro_store::StageId::new("start", 1)).unwrap();
    assert_eq!(
        node.completion.as_ref().unwrap().outcome,
        StageOutcome::Succeeded
    );
}

#[tokio::test]
async fn timeout_causes_fail_status_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Graph::new("timeout_test");

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut work = Node::new("work");
    work.attrs.insert(
        "timeout".to_string(),
        AttrValue::Duration(Duration::from_millis(50)),
    );
    work.attrs
        .insert("type".to_string(), AttrValue::String("slow".to_string()));
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    g.nodes.insert("work".to_string(), work);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.edges.push(Edge::new("start", "work"));
    let mut fail_edge = Edge::new("work", "exit");
    fail_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=failed".to_string()),
    );
    g.edges.push(fail_edge);

    let mut registry = make_registry();
    registry.register("slow", Box::new(SlowHandler { sleep_ms: 500 }));
    let executed = execute_test_run_with_options(
        test_run_options(dir.path(), "test-run"),
        g,
        Some(Arc::new(registry)),
    )
    .await;
    let state = executed.engine.run.run_store.state().await.unwrap();
    let status = state
        .stage(&fabro_store::StageId::new("work", 1))
        .unwrap()
        .completion
        .as_ref()
        .unwrap();
    assert_eq!(status.outcome, StageOutcome::Failed {
        retry_requested: false,
    });

    let events = executed.engine.run.run_store.list_events().await.unwrap();
    let stage_failed = events
        .iter()
        .map(|envelope| &envelope.event)
        .find(|event| {
            event.event_name() == "stage.failed" && event.node_id.as_deref() == Some("work")
        })
        .expect("work stage failed event should be persisted");
    assert_eq!(
        stage_failed.actor,
        Some(Principal::System {
            system_kind: SystemActorKind::Timeout,
        })
    );
}

#[tokio::test]
async fn execute_cancelled_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = simple_graph();
    let mut work = Node::new("work");
    work.attrs
        .insert("type".to_string(), AttrValue::String("slow".to_string()));
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    g.nodes.insert("work".to_string(), work);
    g.edges.clear();
    g.edges.push(Edge::new("start", "work"));
    g.edges.push(Edge::new("work", "exit"));

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cancel_token_clone = cancel_token.clone();
    let mut registry = make_registry();
    registry.register("slow", Box::new(SlowHandler { sleep_ms: 200 }));
    let mut run_options = test_run_options(dir.path(), "test-run");
    run_options.cancel_token = cancel_token;

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_token_clone.cancel();
    });

    let result = run_graph(
        registry,
        test_emitter_arc("test-run"),
        local_env(),
        &g,
        &run_options,
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[tokio::test]
async fn execute_cancelled_mid_run_persists_cancelled_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = simple_graph();
    let mut work = Node::new("work");
    work.attrs
        .insert("type".to_string(), AttrValue::String("slow".to_string()));
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    g.nodes.insert("work".to_string(), work);
    g.edges.clear();
    g.edges.push(Edge::new("start", "work"));
    g.edges.push(Edge::new("work", "exit"));

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cancel_token_clone = cancel_token.clone();
    let mut registry = make_registry();
    registry.register("slow", Box::new(SlowHandler { sleep_ms: 200 }));
    let mut run_options = test_run_options(dir.path(), "test-run");
    run_options.cancel_token = cancel_token;

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_token_clone.cancel();
    });

    let executed = execute_test_run_with_options(run_options, g, Some(Arc::new(registry))).await;

    assert!(matches!(executed.outcome, Err(Error::Cancelled)));
}

#[tokio::test]
async fn max_node_visits_errors_on_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = cyclic_graph();
    g.attrs
        .insert("max_node_visits".to_string(), AttrValue::Integer(3));

    let result = run_graph(
        make_registry(),
        test_emitter_arc("test-run"),
        local_env(),
        &g,
        &test_run_options(dir.path(), "test-run"),
    )
    .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("stuck in a cycle"));
}

#[tokio::test]
async fn panic_handler_returns_panic_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Graph::new("panic_test");
    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);
    let mut panic_node = Node::new("boom");
    panic_node.attrs.insert(
        "type".to_string(),
        AttrValue::String("panicker".to_string()),
    );
    panic_node
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    g.nodes.insert("boom".to_string(), panic_node);
    g.edges.push(Edge::new("start", "boom"));

    let mut registry = make_registry();
    registry.register("panicker", Box::new(PanickingHandler));
    let result = run_graph(
        registry,
        test_emitter_arc("test-run"),
        local_env(),
        &g,
        &test_run_options(dir.path(), "test-run"),
    )
    .await;

    let outcome = result.expect("runner should convert panic into a failed outcome");
    assert_eq!(outcome.status, StageOutcome::Failed {
        retry_requested: false,
    });
}

#[tokio::test]
async fn loop_circuit_breaker_aborts_on_repeated_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = make_registry();
    registry.register("always_fail", Box::new(AlwaysFailHandler));

    let result = run_graph(
        registry,
        test_emitter_arc("test-run"),
        local_env(),
        &looping_fail_graph(),
        &test_run_options(dir.path(), "test-run"),
    )
    .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("deterministic failure cycle detected"));
}

#[tokio::test]
async fn stall_watchdog_triggers_on_hung_handler() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Graph::new("stall_test");
    g.attrs
        .insert("goal".to_string(), AttrValue::String("test".to_string()));
    g.attrs.insert(
        "stall_timeout".to_string(),
        AttrValue::Duration(Duration::from_millis(50)),
    );
    g.attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut work = Node::new("work");
    work.attrs
        .insert("type".to_string(), AttrValue::String("slow".to_string()));
    g.nodes.insert("work".to_string(), work);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.edges.push(Edge::new("start", "work"));
    g.edges.push(Edge::new("work", "exit"));

    let mut registry = make_registry();
    registry.register("slow", Box::new(SlowHandler { sleep_ms: 60_000 }));
    let result = run_graph(
        registry,
        test_emitter_arc("test-run"),
        local_env(),
        &g,
        &test_run_options(dir.path(), "test-run"),
    )
    .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("stall watchdog"));
}

#[tokio::test(start_paused = true)]
async fn stall_watchdog_starts_a_fresh_deadline_after_human_input() {
    let emitter = Arc::new(Emitter::default());
    let blocker = Arc::new(RunInterviewBlocker::new());
    let stall_token = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let watchdog = tokio::spawn(monitor_for_stall(
        Duration::from_millis(50),
        stall_token.clone(),
        shutdown.clone(),
        Arc::clone(&emitter),
        blocker.subscribe(),
    ));
    tokio::task::yield_now().await;

    let guard = blocker.block(Arc::clone(&emitter), fabro_types::StageId::new("work", 1));
    tokio::time::advance(Duration::from_millis(200)).await;
    assert!(!stall_token.is_cancelled());

    guard.resolve();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(49)).await;
    assert!(!stall_token.is_cancelled());

    tokio::time::advance(Duration::from_millis(2)).await;
    tokio::task::yield_now().await;
    assert!(stall_token.is_cancelled());

    shutdown.cancel();
    watchdog.await.unwrap();
}

#[tokio::test]
async fn stall_watchdog_suspends_while_run_waits_for_human_input() {
    let dir = tempfile::tempdir().unwrap();
    // The blocked wait outruns the stall timeout, so this only passes if the
    // watchdog stays suspended and then restarts on a fresh deadline.
    let graph = interview_wait_graph(Duration::from_millis(300), None);
    let mut registry = make_registry();
    registry.register(
        "interview_wait",
        Box::new(InterviewWaitHandler {
            wait_ms:   500,
            active_ms: 10,
        }),
    );

    let outcome = run_graph(
        registry,
        test_emitter_arc("test-run"),
        local_env(),
        &graph,
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .expect("human input wait should not trigger the stall watchdog");

    assert_eq!(outcome.status, StageOutcome::Succeeded);
}

#[tokio::test]
async fn node_timeout_excludes_human_input_wait() {
    let dir = tempfile::tempdir().unwrap();
    // The blocked wait outruns the node timeout, but the active work is well
    // inside it, so this only fails if the interview wait is being charged.
    let graph = interview_wait_graph(Duration::ZERO, Some(Duration::from_millis(300)));
    let mut registry = make_registry();
    registry.register(
        "interview_wait",
        Box::new(InterviewWaitHandler {
            wait_ms:   500,
            active_ms: 20,
        }),
    );

    let outcome = run_graph(
        registry,
        test_emitter_arc("test-run"),
        local_env(),
        &graph,
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .expect("human input wait should not consume the node timeout");

    assert_eq!(outcome.status, StageOutcome::Succeeded);
}

#[tokio::test]
async fn retry_emits_stage_started_per_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Graph::new("retry_events");
    g.attrs
        .insert("goal".to_string(), AttrValue::String("test".to_string()));

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    g.nodes.insert("start".to_string(), start);

    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fail_once".to_string()),
    );
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(1));
    work.attrs.insert(
        "retry_policy".to_string(),
        AttrValue::String("aggressive".to_string()),
    );
    g.nodes.insert("work".to_string(), work);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    g.nodes.insert("exit".to_string(), exit);

    g.edges.push(Edge::new("start", "work"));
    g.edges.push(Edge::new("work", "exit"));

    let events = Arc::new(std::sync::Mutex::new(Vec::<fabro_types::RunEvent>::new()));
    let events_clone = Arc::clone(&events);
    let emitter = test_emitter("retry-events-test");
    emitter.on_event(move |event| {
        events_clone.lock().unwrap().push(event.clone());
    });

    let mut registry = make_registry();
    registry.register(
        "fail_once",
        Box::new(FailOnceThenSucceedHandler {
            call_count: AtomicU32::new(0),
        }),
    );

    let outcome = run_graph(
        registry,
        Arc::new(emitter),
        local_env(),
        &g,
        &test_run_options(dir.path(), "retry-events-test"),
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, StageOutcome::Succeeded);

    let collected = events.lock().unwrap();
    let work_started: Vec<_> = collected
        .iter()
        .filter(|event| {
            event.event_name() == "stage.started" && event.node_id.as_deref() == Some("work")
        })
        .map(|event| event.properties().unwrap()["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(work_started, vec![1, 2]);
}

#[tokio::test]
async fn run_with_lifecycle_emits_initialize_and_setup_events() {
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let events_clone = Arc::clone(&events);
    let emitter = test_emitter("order-test");
    emitter.on_event(move |event| {
        let name = match event.event_name() {
            "sandbox.initialized" => "SandboxInitialized",
            "setup.started" => "SetupStarted",
            "setup.completed" => "SetupCompleted",
            "run.started" => "WorkflowRunStarted",
            "run.running" => "RunRunning",
            _ => return,
        };
        events_clone.lock().unwrap().push(name.to_string());
    });

    let outcome = run_with_lifecycle(
        make_registry(),
        Arc::new(emitter),
        local_env(),
        &simple_graph(),
        test_run_options(dir.path(), "order-test"),
        test_lifecycle(vec!["echo ok"]),
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, StageOutcome::Succeeded);

    let names = events.lock().unwrap();
    let sandbox_idx = names
        .iter()
        .position(|n| n == "SandboxInitialized")
        .unwrap();
    let setup_idx = names.iter().position(|n| n == "SetupStarted").unwrap();
    let run_started_idx = names
        .iter()
        .position(|n| n == "WorkflowRunStarted")
        .unwrap();
    let run_running_idx = names.iter().position(|n| n == "RunRunning").unwrap();
    assert!(sandbox_idx < setup_idx);
    assert!(setup_idx < run_started_idx);
    assert!(run_started_idx < run_running_idx);
}
