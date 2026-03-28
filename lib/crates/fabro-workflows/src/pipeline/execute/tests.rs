use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use fabro_agent::Sandbox;
use fabro_config::FabroSettings;
use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_hooks::HookConfig;
use fabro_interview::AutoApproveInterviewer;

use super::*;
use crate::context::{self, Context};
use crate::error::FabroError;
use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::handler::start::StartHandler;
use crate::handler::{Handler as HandlerTrait, HandlerRegistry};
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use crate::pipeline::initialize;
use crate::pipeline::types::{InitOptions, LlmSpec, Persisted, SandboxEnvSpec, SandboxSpec};
use crate::records::{Checkpoint, CheckpointExt, RunRecord, StartRecordExt};
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

fn test_run_options(run_dir: &Path, run_id: &str) -> RunOptions {
    RunOptions {
        run_dir: run_dir.to_path_buf(),
        cancel_token: None,
        run_id: run_id.into(),
        settings: FabroSettings::default(),
        git: None,
        host_repo_path: None,
        labels: HashMap::new(),
        github_app: None,
        git_author: crate::git::GitAuthor::default(),
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

fn persisted_workflow(graph: Graph, source: String, run_dir: &Path, run_id: &str) -> Persisted {
    Persisted::new(
        graph.clone(),
        source,
        vec![],
        run_dir.to_path_buf(),
        RunRecord {
            run_id: run_id.to_string(),
            created_at: Utc::now(),
            settings: FabroSettings::default(),
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

#[tokio::test]
async fn execute_runs_start_to_exit_and_returns_final_context() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let (graph, source) = simple_validated_graph();
    let initialized = initialize(
        persisted_workflow(graph, source, &run_dir, "run-test"),
        InitOptions {
            run_id: "run-test".to_string(),
            dry_run: false,
            emitter: Arc::new(crate::event::EventEmitter::new()),
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
            hooks: HookConfig { hooks: vec![] },
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
        Some(serde_json::json!("run-test"))
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
    let run_id = run_options.run_id.clone();
    let initialized = initialize(
        persisted_workflow(graph.clone(), String::new(), &run_dir, &run_id),
        InitOptions {
            run_id,
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
            hooks: HookConfig { hooks: vec![] },
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
        Arc::new(EventEmitter::new()),
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
    run_graph(
        make_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
        &simple_graph(),
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .unwrap();
    assert!(dir.path().join("checkpoint.json").exists());
}

#[tokio::test]
async fn execute_emits_events() {
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let emitter = EventEmitter::new();
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
        Arc::new(EventEmitter::new()),
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
    run_graph(
        make_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
        &simple_graph(),
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .unwrap();

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
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

    run_graph(
        make_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
        &g,
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .unwrap();

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"path_b".to_string()));
    assert!(!cp.completed_nodes.contains(&"path_a".to_string()));
}

#[tokio::test]
async fn execute_writes_start_json_and_node_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut run_options = test_run_options(dir.path(), "test-run");
    run_options.git = Some(GitCheckpointOptions {
        base_sha: Some("abc123".into()),
        run_branch: Some("fabro/run/test-run".into()),
        meta_branch: None,
    });

    run_graph(
        make_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
        &simple_graph(),
        &run_options,
    )
    .await
    .unwrap();

    let start = crate::records::StartRecord::load(dir.path()).unwrap();
    assert_eq!(start.run_id, "test-run");
    assert_eq!(start.run_branch.as_deref(), Some("fabro/run/test-run"));
    assert_eq!(start.base_sha.as_deref(), Some("abc123"));

    let status_path = dir.path().join("nodes").join("start").join("status.json");
    assert!(status_path.exists());
}

#[tokio::test]
async fn timeout_causes_fail_status_json() {
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
    run_graph(
        registry,
        Arc::new(EventEmitter::new()),
        local_env(),
        &g,
        &test_run_options(dir.path(), "test-run"),
    )
    .await
    .unwrap();

    let status_path = dir.path().join("nodes").join("work").join("status.json");
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
    assert_eq!(status["status"], "fail");
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
        Arc::new(EventEmitter::new()),
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
        Arc::new(EventEmitter::new()),
        local_env(),
        &g,
        &test_run_options(dir.path(), "test-run"),
    )
    .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("stuck in a cycle"));
}

#[tokio::test]
async fn panic_handler_writes_panic_txt() {
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
    let _ = run_graph(
        registry,
        Arc::new(EventEmitter::new()),
        local_env(),
        &g,
        &test_run_options(dir.path(), "test-run"),
    )
    .await;

    let panic_path = dir.path().join("nodes").join("boom").join("panic.txt");
    assert!(panic_path.exists());
    let content = std::fs::read_to_string(&panic_path).unwrap();
    assert!(content.contains("test panic message"));
}

#[tokio::test]
async fn loop_circuit_breaker_aborts_on_repeated_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = make_registry();
    registry.register("always_fail", Box::new(AlwaysFailHandler));

    let result = run_graph(
        registry,
        Arc::new(EventEmitter::new()),
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
        Arc::new(EventEmitter::new()),
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

    let events = Arc::new(std::sync::Mutex::new(Vec::<WorkflowRunEvent>::new()));
    let events_clone = Arc::clone(&events);
    let emitter = EventEmitter::new();
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
        .filter_map(|e| match e {
            WorkflowRunEvent::StageStarted {
                node_id, attempt, ..
            } if node_id == "work" => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(work_started, vec![1, 2]);
}

#[tokio::test]
async fn run_with_lifecycle_emits_initialize_and_setup_events() {
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let events_clone = Arc::clone(&events);
    let emitter = EventEmitter::new();
    emitter.on_event(move |event| {
        let name = match event {
            WorkflowRunEvent::SandboxInitialized { .. } => "SandboxInitialized",
            WorkflowRunEvent::SetupStarted { .. } => "SetupStarted",
            WorkflowRunEvent::SetupCompleted { .. } => "SetupCompleted",
            WorkflowRunEvent::WorkflowRunStarted { .. } => "WorkflowRunStarted",
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
    assert!(sandbox_idx < setup_idx);
    assert!(setup_idx < run_started_idx);
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

    let events = Arc::new(std::sync::Mutex::new(Vec::<WorkflowRunEvent>::new()));
    let events_clone = Arc::clone(&events);
    let emitter = EventEmitter::new();
    emitter.on_event(move |event| {
        events_clone.lock().unwrap().push(event.clone());
    });

    let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(repo.to_path_buf()));
    let mut run_options = test_run_options(run_tmp.path(), "git-cp-test");
    run_options.git = Some(GitCheckpointOptions {
        base_sha: Some(base_sha),
        run_branch: None,
        meta_branch: Some(crate::git::MetadataStore::branch_name("git-cp-test")),
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
        .filter_map(|e| match e {
            WorkflowRunEvent::CheckpointCompleted {
                node_id,
                git_commit_sha: Some(_),
                ..
            } => Some(node_id.as_str()),
            _ => None,
        })
        .collect();
    assert!(!checkpoint_node_ids.contains(&"start"));
    assert!(checkpoint_node_ids.contains(&"work"));
}
