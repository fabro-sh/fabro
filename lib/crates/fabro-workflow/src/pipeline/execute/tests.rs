#![allow(clippy::absolute_paths, clippy::large_futures)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_hooks::HookSettings;
use fabro_interview::AutoApproveInterviewer;
use fabro_sandbox::SandboxSpec;
use fabro_store::{SlateStore, StoreHandle};
use fabro_types::{RunId, Settings, fixtures};
use object_store::memory::InMemory;

use super::*;
use crate::context::{self, Context};
use crate::error::FabroError;
use crate::event::{EventEmitter, StoreProgressLogger};
use crate::handler::start::StartHandler;
use crate::handler::{Handler as HandlerTrait, HandlerRegistry};
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use crate::pipeline::initialize;
use crate::pipeline::types::{InitOptions, LlmSpec, Persisted, SandboxEnvSpec};
use crate::records::RunRecord;
use crate::run_options::{GitCheckpointOptions, LifecycleOptions, RunOptions};
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

fn test_emitter(label: &str) -> EventEmitter {
    EventEmitter::new(test_run_id(label))
}

fn test_emitter_arc(label: &str) -> Arc<EventEmitter> {
    Arc::new(test_emitter(label))
}

fn test_run_options(run_dir: &Path, run_id: &str) -> RunOptions {
    RunOptions {
        run_dir: run_dir.to_path_buf(),
        cancel_token: None,
        run_id: test_run_id(run_id),
        settings: Settings::default(),
        git: None,
        host_repo_path: None,
        labels: HashMap::new(),
        github_app: None,
        base_branch: None,
        display_base_sha: None,
        workflow_slug: None,
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
        RunRecord {
            run_id,
            settings: Settings::default(),
            graph,
            workflow_slug: Some("test".to_string()),
            working_directory: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            host_repo_path: Some(
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .display()
                    .to_string(),
            ),
            base_branch: Some("main".to_string()),
            labels: HashMap::new(),
        },
    )
}

fn test_lifecycle(setup_commands: Vec<String>) -> LifecycleOptions {
    LifecycleOptions {
        setup_commands,
        setup_command_timeout_ms: 300_000,
        devcontainer_phases: Vec::new(),
    }
}

async fn test_run_store(run_id: &RunId) -> fabro_store::SlateRunStore {
    let store: StoreHandle = Arc::new(SlateStore::new(
        Arc::new(InMemory::new()),
        "",
        Duration::from_millis(1),
    ));
    store.create_run(run_id).await.unwrap()
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
    let emitter = test_emitter_arc("test-run");
    let store_logger = StoreProgressLogger::new(run_store.clone());
    store_logger.register(&emitter);
    let initialized = initialize(
        persisted_workflow(graph, String::new(), &run_options.run_dir, run_id_value),
        InitOptions {
            run_id: run_id_value,
            run_store,
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
            lifecycle: LifecycleOptions {
                setup_commands: vec![],
                setup_command_timeout_ms: 1_000,
                devcontainer_phases: vec![],
            },
            run_options,
            hooks: HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                devcontainer_env: HashMap::new(),
                toml_env: HashMap::new(),
                github_permissions: None,
                origin_url: None,
            },
            devcontainer: None,
            git: git_options,
            worktree_mode: None,
            registry_override,
            checkpoint: None,
            seed_context: None,
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
    let initialized = initialize(
        persisted_workflow(graph, source, &run_dir, test_run_id("run-test")),
        InitOptions {
            run_id: test_run_id("run-test"),
            run_store: test_run_store(&test_run_id("run-test")).await,
            dry_run: false,
            emitter: test_emitter_arc("run-test"),
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
            lifecycle: LifecycleOptions {
                setup_commands: vec![],
                setup_command_timeout_ms: 1_000,
                devcontainer_phases: vec![],
            },
            run_options: test_run_options(&run_dir, "run-test"),
            hooks: HookSettings { hooks: vec![] },
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

    let executed = execute(initialized).await;

    assert_eq!(
        executed.outcome.as_ref().unwrap().status,
        crate::outcome::StageStatus::Success
    );
    assert_eq!(
        executed
            .final_context
            .get(crate::context::keys::INTERNAL_RUN_ID),
        Some(serde_json::json!(test_run_id("run-test").to_string()))
    );
}

async fn run_with_lifecycle(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &Graph,
    run_options: RunOptions,
    lifecycle: LifecycleOptions,
) -> Result<Outcome, FabroError> {
    std::fs::create_dir_all(&run_options.run_dir).unwrap();
    let run_dir = run_options.run_dir.clone();
    let run_id = run_options.run_id;
    let initialized = initialize(
        persisted_workflow(graph.clone(), String::new(), &run_dir, run_id),
        InitOptions {
            run_id,
            run_store: test_run_store(&run_id).await,
            dry_run: false,
            emitter,
            sandbox: SandboxSpec::Local {
                working_directory: PathBuf::from(sandbox.working_directory()),
            },
            llm: LlmSpec {
                model: "test-model".to_string(),
                provider: fabro_llm::Provider::Anthropic,
                fallback_chain: Vec::new(),
                mcp_servers: Vec::new(),
                dry_run: true,
            },
            interviewer: Arc::new(AutoApproveInterviewer),
            lifecycle,
            run_options,
            hooks: HookSettings { hooks: vec![] },
            sandbox_env: SandboxEnvSpec {
                devcontainer_env: HashMap::new(),
                toml_env: HashMap::new(),
                github_permissions: None,
                origin_url: None,
            },
            devcontainer: None,
            git: None,
            worktree_mode: None,
            registry_override: Some(Arc::new(registry)),
            checkpoint: None,
            seed_context: None,
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
    ) -> std::result::Result<Outcome, FabroError> {
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
    ) -> std::result::Result<Outcome, FabroError> {
        tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
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
    ) -> std::result::Result<Outcome, FabroError> {
        panic!("test panic message");
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
    ) -> std::result::Result<Outcome, FabroError> {
        if self.call_count.fetch_add(1, Ordering::Relaxed) == 0 {
            Err(FabroError::handler("transient failure"))
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
        AttrValue::String("outcome=fail".to_string()),
    );
    g.edges.push(fail_edge);
    let mut ok_edge = Edge::new("work", "exit");
    ok_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
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
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
async fn execute_saves_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let executed = execute_test_run(dir.path(), simple_graph(), "test-run").await;
    assert!(
        executed
            .run_store
            .state()
            .await
            .unwrap()
            .checkpoint
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
        .run_store
        .state()
        .await
        .unwrap()
        .checkpoint
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
        AttrValue::String("outcome=fail".to_string()),
    );
    g.edges.push(e1);
    g.edges.push(Edge::new("start", "path_b"));
    g.edges.push(Edge::new("path_a", "exit"));
    g.edges.push(Edge::new("path_b", "exit"));

    let executed = execute_test_run(dir.path(), g, "test-run").await;
    let cp = executed
        .run_store
        .state()
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    assert!(cp.completed_nodes.contains(&"path_b".to_string()));
    assert!(!cp.completed_nodes.contains(&"path_a".to_string()));
}

#[tokio::test]
async fn execute_writes_start_json_and_node_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut run_options = test_run_options(dir.path(), "test-run");
    run_options.git = Some(GitCheckpointOptions {
        base_sha: Some("abc123".into()),
        run_branch: Some(format!("fabro/run/{}", test_run_id("test-run"))),
        meta_branch: None,
    });

    let executed = execute_test_run_with_options(run_options, simple_graph(), None).await;
    let state = executed.run_store.state().await.unwrap();
    let start = state.start.as_ref().unwrap();
    assert_eq!(start.run_id, test_run_id("test-run"));
    assert_eq!(
        start.run_branch.as_deref(),
        Some(format!("fabro/run/{}", test_run_id("test-run")).as_str())
    );
    assert_eq!(start.base_sha.as_deref(), Some("abc123"));

    let node = state.node(&fabro_store::StageId::new("start", 1)).unwrap();
    assert_eq!(node.status.as_ref().unwrap().status, StageStatus::Success);
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
        AttrValue::String("outcome=fail".to_string()),
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
    let state = executed.run_store.state().await.unwrap();
    let status = state
        .node(&fabro_store::StageId::new("work", 1))
        .unwrap()
        .status
        .as_ref()
        .unwrap();
    assert_eq!(status.status, StageStatus::Fail);
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

    let cancel_token = Arc::new(AtomicBool::new(false));
    let cancel_token_clone = Arc::clone(&cancel_token);
    let mut registry = make_registry();
    registry.register("slow", Box::new(SlowHandler { sleep_ms: 200 }));
    let mut run_options = test_run_options(dir.path(), "test-run");
    run_options.cancel_token = Some(cancel_token);

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_token_clone.store(true, Ordering::Relaxed);
    });

    let result = run_graph(
        registry,
        test_emitter_arc("test-run"),
        local_env(),
        &g,
        &run_options,
    )
    .await;
    assert!(matches!(result, Err(FabroError::Cancelled)));
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
    assert_eq!(outcome.status, StageStatus::Fail);
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

    let events = Arc::new(std::sync::Mutex::new(Vec::<fabro_types::StoredEvent>::new()));
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
    assert_eq!(outcome.status, StageStatus::Success);

    let collected = events.lock().unwrap();
    let work_started: Vec<_> = collected
        .iter()
        .filter(|event| {
            event.event_name() == "stage.started" && event.node_id.as_deref() == Some("work")
        })
        .map(|event| event.properties()["attempt"].as_u64().unwrap())
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
        let name = match event.event_name().as_str() {
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
        test_lifecycle(vec!["echo ok".to_string()]),
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, StageStatus::Success);

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

#[tokio::test]
async fn git_checkpoint_skips_start_node() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@test.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    let base_sha = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let run_tmp = tempfile::tempdir().unwrap();
    let mut g = simple_graph();
    g.nodes.insert("work".to_string(), Node::new("work"));
    g.edges.clear();
    g.edges.push(Edge::new("start", "work"));
    g.edges.push(Edge::new("work", "exit"));

    let events = Arc::new(std::sync::Mutex::new(Vec::<fabro_types::StoredEvent>::new()));
    let events_clone = Arc::clone(&events);
    let emitter = test_emitter("git-cp-test");
    emitter.on_event(move |event| {
        events_clone.lock().unwrap().push(event.clone());
    });

    let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(repo.to_path_buf()));
    let mut run_options = test_run_options(run_tmp.path(), "git-cp-test");
    run_options.git = Some(GitCheckpointOptions {
        base_sha: Some(base_sha),
        run_branch: None,
        meta_branch: Some(crate::git::MetadataStore::branch_name(
            &test_run_id("git-cp-test").to_string(),
        )),
    });
    run_options.host_repo_path = Some(repo.to_path_buf());

    run_graph(
        make_registry(),
        Arc::new(emitter),
        sandbox,
        &g,
        &run_options,
    )
    .await
    .unwrap();

    let collected = events.lock().unwrap();
    let checkpoint_node_ids: Vec<&str> = collected
        .iter()
        .filter(|event| {
            event.event_name() == "checkpoint.completed"
                && event
                    .properties()
                    .get("git_commit_sha")
                    .and_then(|value| value.as_str())
                    .is_some()
        })
        .filter_map(|event| event.node_id.as_deref())
        .collect();
    assert!(!checkpoint_node_ids.contains(&"start"));
    assert!(checkpoint_node_ids.contains(&"work"));
}
