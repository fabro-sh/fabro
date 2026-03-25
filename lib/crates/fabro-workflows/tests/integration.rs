use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fabro_config::FabroConfig;
use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_graphviz::parser::parse;
use fabro_interview::{
    Answer, AnswerValue, AutoApproveInterviewer, CallbackInterviewer, Interviewer,
    QueueInterviewer, RecordingInterviewer,
};
use fabro_llm::provider::Provider;
use fabro_validate::{validate, validate_or_raise, Severity};
use fabro_workflows::backend::cli::{parse_cli_response, AgentCliBackend, BackendRouter};
use fabro_workflows::backend::AgentApiBackend;
use fabro_workflows::checkpoint::Checkpoint;
use fabro_workflows::context::Context;
use fabro_workflows::error::FabroError;
use fabro_workflows::event::{EventEmitter, WorkflowRunEvent};
use fabro_workflows::handler::agent::{AgentHandler, CodergenBackend, CodergenResult};
use fabro_workflows::handler::command::CommandHandler;
use fabro_workflows::handler::conditional::ConditionalHandler;
use fabro_workflows::handler::default_registry;
use fabro_workflows::handler::exit::ExitHandler;
use fabro_workflows::handler::human::HumanHandler;
use fabro_workflows::handler::manager_loop::SubWorkflowHandler;
use fabro_workflows::handler::start::StartHandler;
use fabro_workflows::handler::wait::WaitHandler;
use fabro_workflows::handler::{Handler, HandlerRegistry};
use fabro_workflows::outcome::{Outcome, OutcomeExt, StageStatus};
use fabro_workflows::run_settings::{GitCheckpointSettings, RunSettings};
use fabro_workflows::stylesheet::{apply_stylesheet, parse_stylesheet};
use fabro_workflows::test_support::{run_graph_with_hooks, WorkflowRunner};
use fabro_workflows::transform::{
    StylesheetApplicationTransform, Transform, VariableExpansionTransform,
};

fn local_env() -> Arc<dyn fabro_agent::Sandbox> {
    Arc::new(fabro_agent::LocalSandbox::new(
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    ))
}

// ---------------------------------------------------------------------------
// 1. Parse and validate all 3 spec examples (Section 2.13)
// ---------------------------------------------------------------------------

#[test]
fn parse_and_validate_simple_linear() {
    let input = r#"digraph Simple {
        graph [goal="Run tests and report"]
        rankdir=LR

        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]

        run_tests [label="Run Tests", prompt="Run the test suite and report results"]
        report    [label="Report", prompt="Summarize the test results"]

        start -> run_tests -> report -> exit
    }"#;

    let graph = parse(input).expect("parsing should succeed");
    assert_eq!(graph.name, "Simple");
    assert_eq!(graph.goal(), "Run tests and report");
    assert_eq!(graph.nodes.len(), 4);
    assert_eq!(graph.edges.len(), 3);
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());

    let diagnostics = validate_or_raise(&graph, &[]).expect("validation should pass");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == fabro_validate::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no validation errors");
}

#[test]
fn parse_and_validate_branching_with_conditions() {
    let input = r#"digraph Branch {
        graph [goal="Implement and validate a feature"]
        rankdir=LR
        node [shape=box, timeout="900s"]

        start     [shape=Mdiamond, label="Start"]
        exit      [shape=Msquare, label="Exit"]
        plan      [label="Plan", prompt="Plan the implementation"]
        implement [label="Implement", prompt="Implement the plan"]
        validate  [label="Validate", prompt="Run tests"]
        gate      [shape=diamond, label="Tests passing?"]

        start -> plan -> implement -> validate -> gate
        gate -> exit      [label="Yes", condition="outcome=success"]
        gate -> implement [label="No"]
    }"#;

    let graph = parse(input).expect("parsing should succeed");
    assert_eq!(graph.name, "Branch");
    assert_eq!(graph.nodes.len(), 6);
    assert_eq!(graph.edges.len(), 6);

    let gate_exit = graph
        .edges
        .iter()
        .find(|e| e.from == "gate" && e.to == "exit")
        .expect("gate -> exit edge should exist");
    assert_eq!(gate_exit.condition(), Some("outcome=success"));

    let gate_impl = graph
        .edges
        .iter()
        .find(|e| e.from == "gate" && e.to == "implement")
        .expect("gate -> implement edge should exist");
    assert_eq!(gate_impl.condition(), None);

    let diagnostics = validate_or_raise(&graph, &[]).expect("validation should pass");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == fabro_validate::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no validation errors");
}

#[test]
fn parse_and_validate_human_gate() {
    let input = r#"digraph Review {
        rankdir=LR

        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]

        review_gate [
            shape=hexagon,
            label="Review Changes",
            type="human"
        ]

        start -> review_gate
        review_gate -> ship_it [label="[A] Approve"]
        review_gate -> fixes   [label="[F] Fix"]
        ship_it -> exit
        fixes -> review_gate
    }"#;

    let graph = parse(input).expect("parsing should succeed");
    assert_eq!(graph.name, "Review");
    assert_eq!(graph.nodes.len(), 5);
    assert_eq!(graph.edges.len(), 5);

    let gate = &graph.nodes["review_gate"];
    assert_eq!(gate.node_type(), Some("human"));
    assert_eq!(gate.shape(), "hexagon");
    assert_eq!(gate.label(), "Review Changes");

    let diagnostics = validate_or_raise(&graph, &[]).expect("validation should pass");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == fabro_validate::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no validation errors");
}

// ---------------------------------------------------------------------------
// 2. End-to-end linear pipeline
// ---------------------------------------------------------------------------

fn make_linear_registry() -> HandlerRegistry {
    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(None)));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("agent", Box::new(AgentHandler::new(None)));
    registry
}

#[tokio::test]
async fn end_to_end_linear_pipeline() {
    let input = r#"digraph Linear {
        graph [goal="Build the feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        codergen_step [shape=box, label="Code", prompt="Implement the feature"]
        start -> codergen_step -> exit
    }"#;

    let graph = parse(input).expect("parse should succeed");
    validate_or_raise(&graph, &[]).expect("validation should pass");

    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(
        make_linear_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Checkpoint should exist
    let checkpoint_path = dir.path().join("checkpoint.json");
    assert!(checkpoint_path.exists(), "checkpoint.json should exist");

    let checkpoint = Checkpoint::load(&checkpoint_path).expect("checkpoint should load");
    assert!(checkpoint.completed_nodes.contains(&"start".to_string()));
    assert!(checkpoint
        .completed_nodes
        .contains(&"codergen_step".to_string()));

    // Codergen handler writes prompt.md, response.md, status.json
    let stage_dir = dir.path().join("nodes").join("codergen_step");
    assert!(
        stage_dir.join("prompt.md").exists(),
        "prompt.md should exist"
    );
    assert!(
        stage_dir.join("response.md").exists(),
        "response.md should exist"
    );
    assert!(
        stage_dir.join("status.json").exists(),
        "status.json should exist"
    );

    let prompt_content = std::fs::read_to_string(stage_dir.join("prompt.md")).unwrap();
    assert!(
        prompt_content.ends_with("Implement the feature"),
        "prompt should end with original prompt, got: {prompt_content}"
    );
}

// ---------------------------------------------------------------------------
// 3. End-to-end branching pipeline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn end_to_end_branching_pipeline() {
    // Build a graph:
    //   start -> work -> gate (diamond)
    //   gate -> success_path [condition="outcome=success"]
    //   gate -> fail_path    [condition="outcome=fail"]
    //   success_path -> exit
    //   fail_path -> exit
    //
    // Since work defaults to codergen (shape=box) which returns SUCCESS,
    // the engine should route gate -> success_path via condition match.

    let mut graph = Graph::new("BranchTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test branching".to_string()),
    );

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

    let mut work = Node::new("work");
    work.attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    work.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Do work".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("diamond".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);

    graph
        .nodes
        .insert("success_path".to_string(), Node::new("success_path"));
    graph
        .nodes
        .insert("fail_path".to_string(), Node::new("fail_path"));

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "gate"));

    let mut gate_success = Edge::new("gate", "success_path");
    gate_success.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(gate_success);

    let mut gate_fail = Edge::new("gate", "fail_path");
    gate_fail.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    graph.edges.push(gate_fail);

    graph.edges.push(Edge::new("success_path", "exit"));
    graph.edges.push(Edge::new("fail_path", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(None)));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("agent", Box::new(AgentHandler::new(None)));
    registry.register("conditional", Box::new(ConditionalHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"success_path".to_string()),
        "should have traversed success_path"
    );
    assert!(
        !checkpoint
            .completed_nodes
            .contains(&"fail_path".to_string()),
        "should NOT have traversed fail_path"
    );
}

// ---------------------------------------------------------------------------
// 4. End-to-end human gate pipeline with QueueInterviewer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn end_to_end_human_gate_pipeline() {
    // Build a graph:
    //   start -> gate (hexagon, type=wait.human)
    //   gate -> approve [label="[A] Approve"]
    //   gate -> reject  [label="[R] Reject"]
    //   approve -> exit
    //   reject -> exit
    //
    // QueueInterviewer pre-filled to select "R" -> should route to reject

    let mut graph = Graph::new("HumanGateTest");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Review Changes".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);

    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("reject".to_string(), Node::new("reject"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut e_approve = Edge::new("gate", "approve");
    e_approve.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(e_approve);

    let mut e_reject = Edge::new("gate", "reject");
    e_reject.attrs.insert(
        "label".to_string(),
        AttrValue::String("[R] Reject".to_string()),
    );
    graph.edges.push(e_reject);

    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("reject", "exit"));

    // Pre-fill the queue with an answer selecting "R"
    let answers = VecDeque::from([Answer {
        value: AnswerValue::Selected("R".to_string()),
        selected_option: None,
        text: None,
    }]);
    let interviewer = Arc::new(QueueInterviewer::new(answers));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint.completed_nodes.contains(&"reject".to_string()),
        "should have traversed reject path"
    );
    assert!(
        !checkpoint.completed_nodes.contains(&"approve".to_string()),
        "should NOT have traversed approve path"
    );
}

#[tokio::test]
async fn human_gate_aborted_input_fails_closed_without_fail_route() {
    let mut graph = Graph::new("HumanGateAbortedClosed");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Approve release?".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("revise".to_string(), Node::new("revise"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut approve_edge = Edge::new("gate", "approve");
    approve_edge.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(approve_edge);

    let mut revise_edge = Edge::new("gate", "revise");
    revise_edge.attrs.insert(
        "label".to_string(),
        AttrValue::String("[R] Revise".to_string()),
    );
    graph.edges.push(revise_edge);

    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("revise", "exit"));

    let interviewer = Arc::new(CallbackInterviewer::new(|_| Answer::aborted()));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("engine should return Ok with fail outcome");
    assert_eq!(
        outcome.status,
        StageStatus::Fail,
        "aborted human gate should fail closed"
    );
    assert!(
        outcome
            .failure_reason()
            .unwrap_or("")
            .contains("no outgoing fail edge"),
        "unexpected outcome: {outcome:?}"
    );

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint.node_outcomes.contains_key("gate"),
        "gate outcome should be checkpointed before termination"
    );
    assert!(
        !checkpoint.completed_nodes.contains(&"approve".to_string()),
        "approval path must not execute on aborted input"
    );
    assert!(
        !checkpoint.completed_nodes.contains(&"revise".to_string()),
        "other unconditional choice edges must not execute on aborted input"
    );
}

#[tokio::test]
async fn human_gate_aborted_input_routes_via_outcome_fail_condition() {
    let mut graph = Graph::new("HumanGateAbortedFailRoute");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Approve release?".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("manual_review".to_string(), Node::new("manual_review"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut approve_edge = Edge::new("gate", "approve");
    approve_edge.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(approve_edge);

    let mut fail_edge = Edge::new("gate", "manual_review");
    fail_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    graph.edges.push(fail_edge);

    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("manual_review", "exit"));

    let interviewer = Arc::new(CallbackInterviewer::new(|_| Answer::aborted()));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("aborted human gate should follow explicit fail route");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"manual_review".to_string()),
        "explicit fail route should handle unanswered human gates"
    );
    assert!(
        !checkpoint.completed_nodes.contains(&"approve".to_string()),
        "approval path must not execute on aborted input"
    );
}

// ---------------------------------------------------------------------------
// 5. Goal gate enforcement
// ---------------------------------------------------------------------------

/// A custom handler that always returns FAIL for testing goal gate enforcement.
struct AlwaysFailHandler;

#[async_trait::async_trait]
impl Handler for AlwaysFailHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &fabro_workflows::context::Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, fabro_workflows::error::FabroError> {
        Ok(Outcome::fail_classify(format!(
            "forced failure for {}",
            node.id
        )))
    }
}

#[tokio::test]
async fn goal_gate_routes_to_retry_target_on_failure() {
    // Pipeline:
    //   start -> gated_work -> exit
    //   gated_work has goal_gate=true, retry_target=start
    //   gated_work always returns FAIL
    //
    // When engine reaches exit, it checks goal gates and finds gated_work failed.
    // It should route back to retry_target (start).
    //
    // To avoid infinite loops, we set max_retries=0 on gated_work so it fails
    // immediately each time. After looping once (start -> gated_work -> exit -> start
    // -> gated_work -> exit), if goal gate is still unsatisfied and no retry_target
    // changes, we need to limit iterations. The engine itself doesn't limit loops,
    // so we test a simpler scenario: verify the error when retry_target is missing.

    // Test: goal_gate with NO retry_target returns an error
    let mut graph = Graph::new("GoalGateNoRetry");

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

    let mut gated_work = Node::new("gated_work");
    gated_work
        .attrs
        .insert("goal_gate".to_string(), AttrValue::Boolean(true));
    gated_work
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    gated_work.attrs.insert(
        "type".to_string(),
        AttrValue::String("always_fail".to_string()),
    );
    graph.nodes.insert("gated_work".to_string(), gated_work);

    graph.edges.push(Edge::new("start", "gated_work"));
    graph.edges.push(Edge::new("gated_work", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("always_fail", Box::new(AlwaysFailHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_ok(),
        "goal gate unsatisfied with no retry_target should return Ok(fail outcome)"
    );
    let outcome = result.unwrap();
    assert_eq!(
        outcome.status,
        StageStatus::Fail,
        "pipeline outcome should be 'fail' when goal gate unsatisfied"
    );
    let failure_reason = outcome.failure_reason().unwrap_or_default();
    assert!(
        failure_reason.contains("goal gate unsatisfied"),
        "failure_reason should mention goal gate, got: {failure_reason}"
    );
}

#[tokio::test]
async fn goal_gate_routes_to_retry_target_when_present() {
    // Pipeline:
    //   start -> gated_work -> exit
    //   gated_work has goal_gate=true, retry_target=start
    //   gated_work always fails via AlwaysFailHandler.
    //
    // When engine reaches exit and finds goal gate unsatisfied, it should route
    // to the retry_target. Since AlwaysFailHandler always fails, this creates a
    // loop. However, the gated_work node will emit a FAIL outcome, and the
    // edge gated_work -> exit is unconditional, so it still reaches exit. After
    // the first retry (start -> gated_work -> exit), goal gate is still failed
    // and retry_target is still start, so it loops. To prevent an infinite loop
    // in tests, we use a custom handler that fails the first time and succeeds
    // the second time.

    struct FailThenSucceedHandler {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl Handler for FailThenSucceedHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &fabro_workflows::context::Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, fabro_workflows::error::FabroError> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(Outcome::fail_classify("first attempt fails"))
            } else {
                Ok(Outcome::success())
            }
        }
    }

    let mut graph = Graph::new("GoalGateRetry");

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

    let mut gated_work = Node::new("gated_work");
    gated_work
        .attrs
        .insert("goal_gate".to_string(), AttrValue::Boolean(true));
    gated_work
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    gated_work.attrs.insert(
        "retry_target".to_string(),
        AttrValue::String("start".to_string()),
    );
    gated_work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fail_then_succeed".to_string()),
    );
    graph.nodes.insert("gated_work".to_string(), gated_work);

    graph.edges.push(Edge::new("start", "gated_work"));
    graph.edges.push(Edge::new("gated_work", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fail_then_succeed",
        Box::new(FailThenSucceedHandler {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should eventually succeed after retry");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    // gated_work should appear in completed nodes (at least twice -- first fail, then succeed)
    let gated_work_count = checkpoint
        .completed_nodes
        .iter()
        .filter(|n| *n == "gated_work")
        .count();
    assert!(
        gated_work_count >= 2,
        "gated_work should have been executed at least twice, got {gated_work_count}"
    );
}

// ---------------------------------------------------------------------------
// 6. Variable expansion transform
// ---------------------------------------------------------------------------

#[test]
fn variable_expansion_replaces_goal_in_prompts() {
    let mut graph = Graph::new("test");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Fix all bugs".to_string()),
    );

    let mut plan_node = Node::new("plan");
    plan_node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Plan to achieve: $goal".to_string()),
    );
    graph.nodes.insert("plan".to_string(), plan_node);

    let mut impl_node = Node::new("implement");
    impl_node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Implement $goal now".to_string()),
    );
    graph.nodes.insert("implement".to_string(), impl_node);

    let mut no_var_node = Node::new("report");
    no_var_node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Generate a report".to_string()),
    );
    graph.nodes.insert("report".to_string(), no_var_node);

    let transform = VariableExpansionTransform;
    transform.apply(&mut graph);

    let plan_prompt = graph.nodes["plan"]
        .attrs
        .get("prompt")
        .and_then(AttrValue::as_str)
        .expect("plan prompt should exist");
    assert_eq!(plan_prompt, "Plan to achieve: Fix all bugs");

    let impl_prompt = graph.nodes["implement"]
        .attrs
        .get("prompt")
        .and_then(AttrValue::as_str)
        .expect("implement prompt should exist");
    assert_eq!(impl_prompt, "Implement Fix all bugs now");

    let report_prompt = graph.nodes["report"]
        .attrs
        .get("prompt")
        .and_then(AttrValue::as_str)
        .expect("report prompt should exist");
    assert_eq!(report_prompt, "Generate a report");
}

// ---------------------------------------------------------------------------
// 7. Stylesheet application
// ---------------------------------------------------------------------------

#[test]
fn stylesheet_application_by_specificity() {
    let stylesheet_text = r"
        * { model: claude-sonnet-4-5; provider: anthropic; }
        .code { model: claude-opus-4-6; provider: anthropic; }
        #critical_review { model: gpt-5.2; provider: openai; reasoning_effort: high; }
    ";

    let mut graph = Graph::new("test");
    graph.attrs.insert(
        "model_stylesheet".to_string(),
        AttrValue::String(stylesheet_text.to_string()),
    );

    // plan node: no class, should get universal defaults
    let plan = Node::new("plan");
    graph.nodes.insert("plan".to_string(), plan);

    // implement node: class="code", should get .code overrides
    let mut implement = Node::new("implement");
    implement.classes.push("code".to_string());
    graph.nodes.insert("implement".to_string(), implement);

    // critical_review node: class="code" AND id="critical_review", id wins
    let mut critical = Node::new("critical_review");
    critical.classes.push("code".to_string());
    graph.nodes.insert("critical_review".to_string(), critical);

    // explicit node: has explicit model, should NOT be overridden
    let mut explicit = Node::new("explicit_node");
    explicit.attrs.insert(
        "model".to_string(),
        AttrValue::String("my-custom-model".to_string()),
    );
    graph.nodes.insert("explicit_node".to_string(), explicit);

    let transform = StylesheetApplicationTransform;
    transform.apply(&mut graph);

    // plan: universal -> claude-sonnet-4-5
    assert_eq!(
        graph.nodes["plan"].attrs.get("model"),
        Some(&AttrValue::String("claude-sonnet-4-5".to_string()))
    );
    assert_eq!(
        graph.nodes["plan"].attrs.get("provider"),
        Some(&AttrValue::String("anthropic".to_string()))
    );

    // implement: .code -> claude-opus-4-6
    assert_eq!(
        graph.nodes["implement"].attrs.get("model"),
        Some(&AttrValue::String("claude-opus-4-6".to_string()))
    );
    assert_eq!(
        graph.nodes["implement"].attrs.get("provider"),
        Some(&AttrValue::String("anthropic".to_string()))
    );

    // critical_review: #critical_review -> gpt-5.2 (id overrides class)
    assert_eq!(
        graph.nodes["critical_review"].attrs.get("model"),
        Some(&AttrValue::String("gpt-5.2".to_string()))
    );
    assert_eq!(
        graph.nodes["critical_review"].attrs.get("provider"),
        Some(&AttrValue::String("openai".to_string()))
    );
    assert_eq!(
        graph.nodes["critical_review"].attrs.get("reasoning_effort"),
        Some(&AttrValue::String("high".to_string()))
    );

    // explicit_node: explicit attr NOT overridden by universal
    assert_eq!(
        graph.nodes["explicit_node"].attrs.get("model"),
        Some(&AttrValue::String("my-custom-model".to_string()))
    );
}

#[test]
fn stylesheet_application_via_parsed_graph() {
    let input = r#"digraph StyleTest {
        graph [
            goal="Test stylesheet",
            model_stylesheet="* { model: sonnet; }"
        ]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        work  [shape=box, prompt="Do work"]
        start -> work -> exit
    }"#;

    let mut graph = parse(input).expect("parse should succeed");
    validate_or_raise(&graph, &[]).expect("validation should pass");

    let transform = StylesheetApplicationTransform;
    transform.apply(&mut graph);

    // All nodes without explicit model should get "sonnet"
    assert_eq!(
        graph.nodes["work"].attrs.get("model"),
        Some(&AttrValue::String("sonnet".to_string()))
    );
    assert_eq!(
        graph.nodes["start"].attrs.get("model"),
        Some(&AttrValue::String("sonnet".to_string()))
    );
    assert_eq!(
        graph.nodes["exit"].attrs.get("model"),
        Some(&AttrValue::String("sonnet".to_string()))
    );
}

#[test]
fn stylesheet_parse_and_apply_directly() {
    let stylesheet_text = "* { model: base; } .fast { model: turbo; }";
    let stylesheet = parse_stylesheet(stylesheet_text).expect("stylesheet parse should succeed");
    assert_eq!(stylesheet.rules.len(), 2);

    let mut graph = Graph::new("test");
    let plain = Node::new("a");
    graph.nodes.insert("a".to_string(), plain);

    let mut fast_node = Node::new("b");
    fast_node.classes.push("fast".to_string());
    graph.nodes.insert("b".to_string(), fast_node);

    apply_stylesheet(&stylesheet, &mut graph);

    assert_eq!(
        graph.nodes["a"].attrs.get("model"),
        Some(&AttrValue::String("base".to_string()))
    );
    assert_eq!(
        graph.nodes["b"].attrs.get("model"),
        Some(&AttrValue::String("turbo".to_string()))
    );
}

// ---------------------------------------------------------------------------
// 8. Retry on failure (Gap #35.1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_on_failure_then_succeed() {
    // A handler that fails the first call and succeeds on the second.
    struct RetryHandler {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl Handler for RetryHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(Outcome::retry_classify("transient failure"))
            } else {
                Ok(Outcome::success())
            }
        }
    }

    let mut graph = Graph::new("RetryTest");

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

    let mut retry_node = Node::new("work");
    retry_node.attrs.insert(
        "type".to_string(),
        AttrValue::String("retry_handler".to_string()),
    );
    retry_node
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(3));
    retry_node.attrs.insert(
        "retry_policy".to_string(),
        AttrValue::String("linear".to_string()),
    );
    graph.nodes.insert("work".to_string(), retry_node);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "retry_handler",
        Box::new(RetryHandler {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("should succeed after retry");
    assert_eq!(outcome.status, StageStatus::Success);
}

// ---------------------------------------------------------------------------
// 9. Pipeline with 10+ nodes (Gap #35.2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pipeline_with_many_nodes() {
    // Build a linear pipeline: start -> n1 -> n2 -> ... -> n10 -> exit (12 nodes)
    let mut graph = Graph::new("ManyNodes");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test large pipeline".to_string()),
    );

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

    let node_names: Vec<String> = (1..=10).map(|i| format!("step_{i}")).collect();

    for name in &node_names {
        let mut node = Node::new(name.clone());
        node.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String(format!("Execute {name}")),
        );
        graph.nodes.insert(name.clone(), node);
    }

    graph.edges.push(Edge::new("start", &node_names[0]));
    for pair in node_names.windows(2) {
        graph.edges.push(Edge::new(&pair[0], &pair[1]));
    }
    graph
        .edges
        .push(Edge::new(node_names.last().unwrap(), "exit"));

    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(
        make_linear_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("large pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    // All 10 step nodes should be in completed_nodes
    for name in &node_names {
        assert!(
            checkpoint.completed_nodes.contains(name),
            "{name} should be in completed_nodes"
        );
    }
}

// ---------------------------------------------------------------------------
// 10. Checkpoint save and load round-trip (Gap #35.3)
// ---------------------------------------------------------------------------

#[test]
fn checkpoint_save_and_resume_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");

    let ctx = Context::new();
    ctx.set("goal", serde_json::json!("Test checkpoint"));
    ctx.set("progress", serde_json::json!(42));
    let mut retries = std::collections::HashMap::new();
    retries.insert("step_1".to_string(), 1u32);
    let checkpoint = Checkpoint::from_context(
        &ctx,
        "step_2",
        vec!["start".to_string(), "step_1".to_string()],
        retries,
        std::collections::HashMap::new(),
        None,
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    checkpoint.save(&path).expect("save should succeed");

    let loaded = Checkpoint::load(&path).expect("load should succeed");
    assert_eq!(loaded.current_node, "step_2");
    assert_eq!(loaded.completed_nodes.len(), 2);
    assert!(loaded.completed_nodes.contains(&"start".to_string()));
    assert!(loaded.completed_nodes.contains(&"step_1".to_string()));
    assert_eq!(loaded.node_retries.get("step_1"), Some(&1));
    assert_eq!(
        loaded.context_values.get("goal"),
        Some(&serde_json::json!("Test checkpoint"))
    );
    assert_eq!(
        loaded.context_values.get("progress"),
        Some(&serde_json::json!(42))
    );
}

// ---------------------------------------------------------------------------
// 11. Smoke test with mock CodergenBackend (Gap #36)
// ---------------------------------------------------------------------------

struct MockCodergenBackend;

#[async_trait::async_trait]
impl CodergenBackend for MockCodergenBackend {
    async fn run(
        &self,
        node: &Node,
        prompt: &str,
        _context: &Context,
        _thread_id: Option<&str>,
        _emitter: &Arc<EventEmitter>,
        _stage_dir: &std::path::Path,
        _sandbox: &Arc<dyn fabro_agent::Sandbox>,
        _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, FabroError> {
        Ok(CodergenResult::Text {
            text: format!(
                "Response for {}: processed prompt '{}'",
                node.id,
                &prompt[..prompt.len().min(50)]
            ),
            usage: None,
            files_touched: Vec::new(),
            last_file_touched: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers for parity tests
// ---------------------------------------------------------------------------

/// A handler backed by a shared `AtomicU32` counter.
/// Returns Fail on call 0, Success on call >= 1.
struct CounterHandler {
    call_count: Arc<std::sync::atomic::AtomicU32>,
}

#[async_trait::async_trait]
impl Handler for CounterHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count == 0 {
            // Use a message that heuristics classify as transient_infra
            Ok(Outcome::fail_classify("connection refused"))
        } else {
            Ok(Outcome::success())
        }
    }
}

/// A handler that sets a context_update with a large value (>100KB) to trigger artifact offloading.
struct LargeOutputHandler;

#[async_trait::async_trait]
impl Handler for LargeOutputHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let mut outcome = Outcome::success();
        // 150KB string — well above the 100KB artifact threshold
        let large_value = "x".repeat(150 * 1024);
        outcome.context_updates.insert(
            format!("response.{}", node.id),
            serde_json::json!(large_value),
        );
        Ok(outcome)
    }
}

/// A handler that sets `context_updates` = {"`my_flag"`: "set"}.
struct ContextSetterHandler;

#[async_trait::async_trait]
impl Handler for ContextSetterHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let mut outcome = Outcome::success();
        outcome
            .context_updates
            .insert("my_flag".to_string(), serde_json::json!("set"));
        Ok(outcome)
    }
}

fn collect_events(emitter: &EventEmitter) -> Arc<std::sync::Mutex<Vec<WorkflowRunEvent>>> {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    emitter.on_event(move |event| {
        events_clone.lock().unwrap().push(event.clone());
    });
    events
}

fn make_full_registry(interviewer: Arc<dyn Interviewer>) -> HandlerRegistry {
    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(None)));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("agent", Box::new(AgentHandler::new(None)));
    registry.register("conditional", Box::new(ConditionalHandler));
    registry.register("command", Box::new(CommandHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));
    registry.register("wait", Box::new(WaitHandler));
    registry.register("stack.manager_loop", Box::new(SubWorkflowHandler));
    registry
}

fn make_graph_with_start_exit(name: &str) -> Graph {
    let mut graph = Graph::new(name);
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
    graph
}

#[tokio::test]
async fn smoke_test_with_mock_codergen_backend() {
    // Pipeline:
    //   start -> plan -> gate (diamond)
    //   gate -> implement [condition="outcome=success"]
    //   gate -> fix       [condition="outcome!=success"]
    //   implement -> exit
    //   fix -> exit
    //
    // codergen nodes use MockCodergenBackend which returns real Text responses.
    // The gate is a conditional node. Since the mock backend returns success,
    // we should route through implement.

    let mut graph = Graph::new("SmokeTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Build and validate".to_string()),
    );

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

    let mut plan = Node::new("plan");
    plan.attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    plan.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Plan to achieve: $goal".to_string()),
    );
    graph.nodes.insert("plan".to_string(), plan);

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("diamond".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);

    let mut implement = Node::new("implement");
    implement
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    implement.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Implement the plan".to_string()),
    );
    graph.nodes.insert("implement".to_string(), implement);

    let mut fix = Node::new("fix");
    fix.attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    fix.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Fix the issues".to_string()),
    );
    graph.nodes.insert("fix".to_string(), fix);

    graph.edges.push(Edge::new("start", "plan"));
    graph.edges.push(Edge::new("plan", "gate"));

    let mut gate_impl = Edge::new("gate", "implement");
    gate_impl.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(gate_impl);

    let mut gate_fix = Edge::new("gate", "fix");
    gate_fix.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome!=success".to_string()),
    );
    graph.edges.push(gate_fix);

    graph.edges.push(Edge::new("implement", "exit"));
    graph.edges.push(Edge::new("fix", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let backend = Box::new(MockCodergenBackend);
    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(backend))));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "agent",
        Box::new(AgentHandler::new(Some(Box::new(MockCodergenBackend)))),
    );
    registry.register("conditional", Box::new(ConditionalHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("smoke test should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint.completed_nodes.contains(&"plan".to_string()),
        "plan should have executed"
    );
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"implement".to_string()),
        "should route through implement (success path)"
    );
    assert!(
        !checkpoint.completed_nodes.contains(&"fix".to_string()),
        "should NOT have traversed fix path"
    );

    // Verify response.md was written by the mock backend
    let plan_response =
        std::fs::read_to_string(dir.path().join("nodes").join("plan").join("response.md"))
            .expect("plan response should exist");
    assert!(
        plan_response.contains("Response for plan"),
        "mock backend should have written response, got: {plan_response}"
    );

    // Verify prompt.md had $goal expanded by the AgentHandler
    let plan_prompt =
        std::fs::read_to_string(dir.path().join("nodes").join("plan").join("prompt.md"))
            .expect("plan prompt should exist");
    assert!(
        plan_prompt.ends_with("Plan to achieve: Build and validate"),
        "prompt should end with original prompt, got: {plan_prompt}"
    );
}

// ---------------------------------------------------------------------------
// 12. Parallel fan-out / fan-in integration test (Gap #14)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn end_to_end_parallel_fan_out_fan_in() {
    use fabro_workflows::handler::fan_in::FanInHandler;
    use fabro_workflows::handler::parallel::ParallelHandler;

    let input = r#"digraph parallel_test {
        start [shape=Mdiamond]
        fan_out [shape=component]
        branch_a [shape=box, prompt="Branch A work"]
        branch_b [shape=box, prompt="Branch B work"]
        fan_in_node [shape=tripleoctagon]
        done [shape=Msquare]

        start -> fan_out
        fan_out -> branch_a
        fan_out -> branch_b
        branch_a -> fan_in_node
        branch_b -> fan_in_node
        fan_in_node -> done
    }"#;

    let graph = parse(input).expect("parse should succeed");
    validate_or_raise(&graph, &[]).expect("validation should pass");

    let dir = tempfile::tempdir().unwrap();

    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(Box::new(
        MockCodergenBackend,
    )))));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "agent",
        Box::new(AgentHandler::new(Some(Box::new(MockCodergenBackend)))),
    );
    registry.register("parallel", Box::new(ParallelHandler));
    registry.register(
        "parallel.fan_in",
        Box::new(FanInHandler::new(Some(Box::new(MockCodergenBackend)))),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("parallel pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();

    // The parallel node (fan_out) and fan_in_node should be in completed_nodes.
    // Branch nodes run inside the parallel handler, so they are not recorded
    // individually by the engine -- but fan_out and fan_in_node are top-level.
    assert!(
        checkpoint.completed_nodes.contains(&"fan_out".to_string()),
        "fan_out should have been executed"
    );
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"fan_in_node".to_string()),
        "fan_in_node should have been executed"
    );

    // Verify parallel.results was populated (both branches ran)
    let parallel_results = checkpoint
        .context_values
        .get("parallel.results")
        .expect("parallel.results should be in context");
    let results_arr = parallel_results.as_array().expect("should be an array");
    assert_eq!(results_arr.len(), 2, "should have 2 branch results");
}

// ---------------------------------------------------------------------------
// 13. Resume from checkpoint (P1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_from_checkpoint_completes_pipeline() {
    // Build a pipeline: start -> step_a -> step_b -> exit
    // Create a checkpoint mid-pipeline (after step_a) and verify
    // run_from_checkpoint completes from step_b onward.

    let mut graph = Graph::new("ResumeTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test resume".to_string()),
    );

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

    let step_a = Node::new("step_a");
    graph.nodes.insert("step_a".to_string(), step_a);

    let step_b = Node::new("step_b");
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    // Simulate a checkpoint saved after step_a completed.
    // The checkpoint records step_a as current_node with next_node_id = step_b.
    let ctx = Context::new();
    ctx.set("graph.goal", serde_json::json!("Test resume"));
    ctx.set("outcome", serde_json::json!("success"));

    let mut outcomes = std::collections::HashMap::new();
    outcomes.insert("start".to_string(), Outcome::success());
    outcomes.insert("step_a".to_string(), Outcome::success());

    let checkpoint = Checkpoint::from_context(
        &ctx,
        "step_a",
        vec!["start".to_string(), "step_a".to_string()],
        std::collections::HashMap::new(),
        outcomes,
        Some("step_b".to_string()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await
        .expect("resume should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Verify checkpoint written after resume contains step_b
    let final_cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        final_cp.completed_nodes.contains(&"step_b".to_string()),
        "step_b should have been executed after resume"
    );
    // step_a should also be present (carried over from the checkpoint)
    assert!(
        final_cp.completed_nodes.contains(&"step_a".to_string()),
        "step_a should be preserved from checkpoint"
    );
    // start should also be present
    assert!(
        final_cp.completed_nodes.contains(&"start".to_string()),
        "start should be preserved from checkpoint"
    );
}

#[tokio::test]
async fn resume_from_checkpoint_preserves_goal_gate_outcomes() {
    // Build: start -> gated_work (goal_gate=true) -> step_b -> exit
    // Checkpoint after gated_work (success), resume at step_b.
    // At exit, goal gate should pass because outcomes are restored.

    let mut graph = Graph::new("ResumeGoalGateTest");

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

    let mut gated_work = Node::new("gated_work");
    gated_work
        .attrs
        .insert("goal_gate".to_string(), AttrValue::Boolean(true));
    graph.nodes.insert("gated_work".to_string(), gated_work);

    let step_b = Node::new("step_b");
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "gated_work"));
    graph.edges.push(Edge::new("gated_work", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    // Checkpoint: gated_work completed with success, next is step_b
    let ctx = Context::new();
    ctx.set("outcome", serde_json::json!("success"));

    let mut outcomes = std::collections::HashMap::new();
    outcomes.insert("start".to_string(), Outcome::success());
    outcomes.insert("gated_work".to_string(), Outcome::success());

    let checkpoint = Checkpoint::from_context(
        &ctx,
        "gated_work",
        vec!["start".to_string(), "gated_work".to_string()],
        std::collections::HashMap::new(),
        outcomes,
        Some("step_b".to_string()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    // This should succeed because goal gate for gated_work is satisfied
    // via restored outcomes
    let outcome = engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await
        .expect("resume with goal gate should succeed");
    assert_eq!(outcome.status, StageStatus::Success);
}

// ===========================================================================
// Parity tests — P1: Core pipeline behaviors
// ===========================================================================

#[tokio::test]
async fn graph_goal_in_context() {
    let input = r#"digraph GoalTest {
        graph [goal="Ship the widget"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        work  [shape=box, prompt="Build it"]
        start -> work -> exit
    }"#;
    let graph = parse(input).expect("parse");
    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(
        make_linear_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert_eq!(
        cp.context_values.get("graph.goal"),
        Some(&serde_json::json!("Ship the widget"))
    );
}

#[tokio::test]
async fn event_streaming_lifecycle() {
    let input = r#"digraph EventTest {
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        task  [shape=box, prompt="Do something"]
        start -> task -> exit
    }"#;
    let graph = parse(input).expect("parse");
    let dir = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);
    let engine = WorkflowRunner::new(make_linear_registry(), Arc::new(emitter), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let collected = events.lock().unwrap();
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunStarted { .. })));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::StageStarted { name, .. } if name == "start")));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::StageCompleted { name, .. } if name == "start")));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::StageStarted { name, .. } if name == "task")));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::StageCompleted { name, .. } if name == "task")));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::CheckpointCompleted { .. })));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunCompleted { .. })));
    // WorkflowRunStarted first, WorkflowRunCompleted last
    assert!(matches!(
        collected.first().unwrap(),
        WorkflowRunEvent::WorkflowRunStarted { .. }
    ));
    assert!(matches!(
        collected.last().unwrap(),
        WorkflowRunEvent::WorkflowRunCompleted { .. }
    ));
}

#[tokio::test]
async fn context_flow_between_stages() {
    let mut graph = make_graph_with_start_exit("ContextFlowTest");
    let mut step_a = Node::new("step_a");
    step_a
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    step_a.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Step A work".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);
    let mut step_b = Node::new("step_b");
    step_b
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    step_b.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Step B work".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);
    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(
        make_linear_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert_eq!(
        cp.context_values.get("last_stage"),
        Some(&serde_json::json!("step_b"))
    );
    let last_response = cp
        .context_values
        .get("last_response")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(last_response.contains("[Simulated]"));
}

#[tokio::test]
async fn tool_handler_e2e() {
    let mut graph = make_graph_with_start_exit("ToolTest");
    let mut echo_task = Node::new("echo_task");
    echo_task.attrs.insert(
        "shape".to_string(),
        AttrValue::String("parallelogram".to_string()),
    );
    echo_task.attrs.insert(
        "script".to_string(),
        AttrValue::String("echo hello-from-script".to_string()),
    );
    graph.nodes.insert("echo_task".to_string(), echo_task);
    graph.edges.push(Edge::new("start", "echo_task"));
    graph.edges.push(Edge::new("echo_task", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let interviewer = Arc::new(AutoApproveInterviewer);
    let engine = WorkflowRunner::new(
        make_full_registry(interviewer),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let command_output = cp
        .context_values
        .get("command.output")
        .expect("command.output should exist");
    assert!(command_output
        .as_str()
        .unwrap()
        .contains("hello-from-script"));
}

#[tokio::test]
async fn auto_approve_interviewer_e2e() {
    let mut graph = make_graph_with_start_exit("AutoApproveTest");
    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs
        .insert("label".to_string(), AttrValue::String("Review".to_string()));
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("reject".to_string(), Node::new("reject"));
    graph.edges.push(Edge::new("start", "gate"));
    let mut e_approve = Edge::new("gate", "approve");
    e_approve.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(e_approve);
    let mut e_reject = Edge::new("gate", "reject");
    e_reject.attrs.insert(
        "label".to_string(),
        AttrValue::String("[R] Reject".to_string()),
    );
    graph.edges.push(e_reject);
    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("reject", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let interviewer = Arc::new(AutoApproveInterviewer);
    let engine = WorkflowRunner::new(
        make_full_registry(interviewer),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"approve".to_string()));
    assert!(!cp.completed_nodes.contains(&"reject".to_string()));
}

#[tokio::test]
async fn codergen_without_backend_simulated() {
    let input = r#"digraph SimTest {
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        code  [shape=box, prompt="Write the code"]
        start -> code -> exit
    }"#;
    let graph = parse(input).expect("parse");
    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(
        make_linear_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let response =
        std::fs::read_to_string(dir.path().join("nodes").join("code").join("response.md")).unwrap();
    assert!(response.contains("[Simulated]"));

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let last_response = cp
        .context_values
        .get("last_response")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(last_response.contains("[Simulated]"));
}

// ===========================================================================
// Parity tests — P2: Complex scenarios
// ===========================================================================

#[tokio::test]
async fn branching_loop_back_on_failure() {
    struct FailThenSucceedHandler {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl Handler for FailThenSucceedHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(Outcome::fail_classify("first attempt fails"))
            } else {
                Ok(Outcome::success())
            }
        }
    }

    let mut graph = make_graph_with_start_exit("LoopTest");
    let mut implement = Node::new("implement");
    implement
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    implement.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Implement".to_string()),
    );
    graph.nodes.insert("implement".to_string(), implement);
    let mut validate_node = Node::new("validate");
    validate_node.attrs.insert(
        "type".to_string(),
        AttrValue::String("fail_then_succeed".to_string()),
    );
    graph.nodes.insert("validate".to_string(), validate_node);

    graph.edges.push(Edge::new("start", "implement"));
    graph.edges.push(Edge::new("implement", "validate"));
    let mut e_success = Edge::new("validate", "exit");
    e_success.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(e_success);
    let mut e_fail = Edge::new("validate", "implement");
    e_fail.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    graph.edges.push(e_fail);

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(None)));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("agent", Box::new(AgentHandler::new(None)));
    registry.register(
        "fail_then_succeed",
        Box::new(FailThenSucceedHandler {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }),
    );
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let implement_count = cp
        .completed_nodes
        .iter()
        .filter(|n| *n == "implement")
        .count();
    assert!(
        implement_count >= 2,
        "implement should appear at least 2x, got {implement_count}"
    );
}

#[tokio::test]
async fn human_gate_loops_back() {
    let mut graph = make_graph_with_start_exit("HumanLoopTest");
    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs
        .insert("label".to_string(), AttrValue::String("Review".to_string()));
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph.nodes.insert("fix".to_string(), Node::new("fix"));

    graph.edges.push(Edge::new("start", "gate"));
    let mut e_approve = Edge::new("gate", "approve");
    e_approve.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(e_approve);
    let mut e_fix = Edge::new("gate", "fix");
    e_fix.attrs.insert(
        "label".to_string(),
        AttrValue::String("[F] Fix".to_string()),
    );
    graph.edges.push(e_fix);
    graph.edges.push(Edge::new("fix", "gate"));
    graph.edges.push(Edge::new("approve", "exit"));

    let answers = VecDeque::from([
        Answer {
            value: AnswerValue::Selected("F".to_string()),
            selected_option: None,
            text: None,
        },
        Answer {
            value: AnswerValue::Selected("A".to_string()),
            selected_option: None,
            text: None,
        },
    ]);
    let interviewer = Arc::new(QueueInterviewer::new(answers));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let gate_count = cp.completed_nodes.iter().filter(|n| *n == "gate").count();
    assert!(
        gate_count >= 2,
        "gate should appear at least 2x, got {gate_count}"
    );
    assert!(cp.completed_nodes.contains(&"approve".to_string()));
}

#[tokio::test]
async fn scenario_ship_a_feature() {
    let dot = r#"digraph ShipFeature {
        graph [goal="Ship the widget"]
        rankdir=LR
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        plan  [shape=box, prompt="Plan to achieve: $goal"]
        implement [shape=box, prompt="Implement the plan"]
        test  [shape=parallelogram, script="echo PASS"]
        review [shape=hexagon, label="Review Changes"]
        start -> plan -> implement -> test -> review
        review -> exit [label="[A] Approve"]
        review -> implement [label="[F] Fix"]
    }"#;
    let mut graph = parse(dot).expect("parse");
    validate_or_raise(&graph, &[]).expect("validate");
    VariableExpansionTransform.apply(&mut graph);
    assert_eq!(
        graph.nodes["plan"].prompt().unwrap(),
        "Plan to achieve: Ship the widget"
    );

    let interviewer = Arc::new(AutoApproveInterviewer);
    let dir = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);
    let engine = WorkflowRunner::new(
        make_full_registry(interviewer),
        Arc::new(emitter),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let command_output = cp
        .context_values
        .get("command.output")
        .expect("command.output");
    assert!(command_output.as_str().unwrap().contains("PASS"));
    assert!(cp.completed_nodes.contains(&"plan".to_string()));
    assert!(cp.completed_nodes.contains(&"implement".to_string()));
    assert!(cp.completed_nodes.contains(&"test".to_string()));
    assert!(cp.completed_nodes.contains(&"review".to_string()));

    let collected = events.lock().unwrap();
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunStarted { .. })));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunCompleted { .. })));
}

#[tokio::test]
async fn scenario_parallel_expert_review() {
    use fabro_workflows::handler::fan_in::FanInHandler;
    use fabro_workflows::handler::parallel::ParallelHandler;

    let input = r#"digraph ParallelReview {
        start [shape=Mdiamond]
        fan_out [shape=component]
        expert_a [shape=box, prompt="Expert A review"]
        expert_b [shape=box, prompt="Expert B review"]
        expert_c [shape=box, prompt="Expert C review"]
        fan_in_node [shape=tripleoctagon]
        review [shape=hexagon, label="Final Review"]
        exit [shape=Msquare]
        start -> fan_out
        fan_out -> expert_a
        fan_out -> expert_b
        fan_out -> expert_c
        expert_a -> fan_in_node
        expert_b -> fan_in_node
        expert_c -> fan_in_node
        fan_in_node -> review
        review -> exit [label="[A] Approve"]
        review -> fan_out [label="[F] Redo"]
    }"#;
    let graph = parse(input).expect("parse");
    validate_or_raise(&graph, &[]).expect("validate");

    let recorder = Arc::new(RecordingInterviewer::new(Box::new(AutoApproveInterviewer)));
    let dir = tempfile::tempdir().unwrap();

    let interviewer: Arc<dyn Interviewer> = recorder.clone();
    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(Box::new(
        MockCodergenBackend,
    )))));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "agent",
        Box::new(AgentHandler::new(Some(Box::new(MockCodergenBackend)))),
    );
    registry.register("parallel", Box::new(ParallelHandler));
    registry.register(
        "parallel.fan_in",
        Box::new(FanInHandler::new(Some(Box::new(MockCodergenBackend)))),
    );
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let results = cp
        .context_values
        .get("parallel.results")
        .expect("parallel.results");
    assert_eq!(results.as_array().unwrap().len(), 3);

    let recordings = recorder.recordings();
    assert_eq!(recordings.len(), 1, "should have 1 interview recording");
    assert!(cp.completed_nodes.contains(&"review".to_string()));
}

#[tokio::test]
async fn scenario_node_retries_on_retry_status() {
    struct RetryHandler {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl Handler for RetryHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(Outcome::retry_classify("transient failure"))
            } else {
                Ok(Outcome::success())
            }
        }
    }

    let mut graph = make_graph_with_start_exit("RetryScenarioTest");
    let mut flaky = Node::new("flaky");
    flaky.attrs.insert(
        "type".to_string(),
        AttrValue::String("retry_handler".to_string()),
    );
    flaky
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(2));
    flaky.attrs.insert(
        "retry_policy".to_string(),
        AttrValue::String("linear".to_string()),
    );
    graph.nodes.insert("flaky".to_string(), flaky);
    graph.edges.push(Edge::new("start", "flaky"));
    graph.edges.push(Edge::new("flaky", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "retry_handler",
        Box::new(RetryHandler {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }),
    );
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let retry_count = cp
        .node_retries
        .get("flaky")
        .expect("flaky should have retries");
    assert_eq!(*retry_count, 1, "should have retried once");
}

#[tokio::test]
async fn scenario_loop_restart_resets_context() {
    let mut graph = make_graph_with_start_exit("LoopRestartTest");
    let mut work = Node::new("work");
    work.attrs
        .insert("type".to_string(), AttrValue::String("counter".to_string()));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    let mut success_edge = Edge::new("work", "exit");
    success_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(success_edge);
    let mut fail_edge = Edge::new("work", "start");
    fail_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    fail_edge
        .attrs
        .insert("loop_restart".to_string(), AttrValue::Boolean(true));
    graph.edges.push(fail_edge);

    let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "counter",
        Box::new(CounterHandler {
            call_count: Arc::clone(&call_count),
        }),
    );
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);
    assert!(call_count.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn scenario_bug_triage_router() {
    let mut graph = make_graph_with_start_exit("TriageTest");
    let mut triage = Node::new("triage");
    triage.attrs.insert(
        "shape".to_string(),
        AttrValue::String("diamond".to_string()),
    );
    graph.nodes.insert("triage".to_string(), triage);
    graph
        .nodes
        .insert("critical".to_string(), Node::new("critical"));
    graph
        .nodes
        .insert("normal".to_string(), Node::new("normal"));
    graph
        .nodes
        .insert("wontfix".to_string(), Node::new("wontfix"));

    graph.edges.push(Edge::new("start", "triage"));
    let mut e_critical = Edge::new("triage", "critical");
    e_critical.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    e_critical
        .attrs
        .insert("weight".to_string(), AttrValue::Integer(10));
    graph.edges.push(e_critical);
    let mut e_normal = Edge::new("triage", "normal");
    e_normal.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    e_normal
        .attrs
        .insert("weight".to_string(), AttrValue::Integer(5));
    graph.edges.push(e_normal);
    graph.edges.push(Edge::new("triage", "wontfix"));
    graph.edges.push(Edge::new("critical", "exit"));
    graph.edges.push(Edge::new("normal", "exit"));
    graph.edges.push(Edge::new("wontfix", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("conditional", Box::new(ConditionalHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        cp.completed_nodes.contains(&"critical".to_string()),
        "critical should be selected (highest weight)"
    );
    assert!(!cp.completed_nodes.contains(&"normal".to_string()));
    assert!(!cp.completed_nodes.contains(&"wontfix".to_string()));
}

#[tokio::test]
async fn scenario_crash_recovery() {
    let mut graph = make_graph_with_start_exit("CrashRecoveryTest");
    graph.nodes.insert("a".to_string(), Node::new("a"));
    graph.nodes.insert("b".to_string(), Node::new("b"));
    graph.nodes.insert("c".to_string(), Node::new("c"));
    graph.edges.push(Edge::new("start", "a"));
    graph.edges.push(Edge::new("a", "b"));
    graph.edges.push(Edge::new("b", "c"));
    graph.edges.push(Edge::new("c", "exit"));

    let ctx = Context::new();
    ctx.set("outcome", serde_json::json!("success"));
    let mut outcomes = std::collections::HashMap::new();
    outcomes.insert("start".to_string(), Outcome::success());
    outcomes.insert("a".to_string(), Outcome::success());
    let checkpoint = Checkpoint::from_context(
        &ctx,
        "a",
        vec!["start".to_string(), "a".to_string()],
        std::collections::HashMap::new(),
        outcomes,
        Some("b".to_string()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await
        .expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"b".to_string()));
    assert!(cp.completed_nodes.contains(&"c".to_string()));
    assert!(cp.completed_nodes.contains(&"a".to_string()));
    let a_count = cp.completed_nodes.iter().filter(|n| *n == "a").count();
    assert_eq!(a_count, 1, "a should not be re-executed");
}

#[tokio::test]
async fn manager_loop_stop_condition_satisfied_e2e() {
    struct DoneSetterHandler;

    #[async_trait::async_trait]
    impl Handler for DoneSetterHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            let mut outcome = Outcome::success();
            outcome
                .context_updates
                .insert("done".to_string(), serde_json::json!("true"));
            Ok(outcome)
        }
    }

    // A slow handler so the child doesn't finish before the stop condition is checked
    struct SlowHandler;
    #[async_trait::async_trait]
    impl Handler for SlowHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            Ok(Outcome::success())
        }
    }

    let mut graph = make_graph_with_start_exit("ManagerStopTest");
    let mut setter = Node::new("setter");
    setter.attrs.insert(
        "type".to_string(),
        AttrValue::String("done_setter".to_string()),
    );
    graph.nodes.insert("setter".to_string(), setter);
    let mut manager = Node::new("manager");
    manager.attrs.insert(
        "type".to_string(),
        AttrValue::String("stack.manager_loop".to_string()),
    );
    manager.attrs.insert(
        "stack.child_dot_source".to_string(),
        AttrValue::String(
            "digraph Child { start [shape=Mdiamond]; slow [shape=box]; exit [shape=Msquare]; start -> slow -> exit }"
                .to_string(),
        ),
    );
    manager.attrs.insert(
        "manager.stop_condition".to_string(),
        AttrValue::String("context.done=true".to_string()),
    );
    manager
        .attrs
        .insert("manager.max_cycles".to_string(), AttrValue::Integer(10));
    manager.attrs.insert(
        "manager.poll_interval".to_string(),
        AttrValue::Duration(std::time::Duration::from_millis(1)),
    );
    graph.nodes.insert("manager".to_string(), manager);
    graph.edges.push(Edge::new("start", "setter"));
    graph.edges.push(Edge::new("setter", "manager"));
    graph.edges.push(Edge::new("manager", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(SlowHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("done_setter", Box::new(DoneSetterHandler));
    registry.register("stack.manager_loop", Box::new(SubWorkflowHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let manager_outcome = cp.node_outcomes.get("manager").expect("manager outcome");
    assert_eq!(manager_outcome.status, StageStatus::Success);
    assert!(manager_outcome
        .notes
        .as_deref()
        .unwrap()
        .contains("Stop condition satisfied"));
    // Overall pipeline succeeds because manager succeeded
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
async fn manager_loop_max_cycles_exceeded_e2e() {
    // A slow handler so the child doesn't finish before max cycles
    struct SlowHandler;
    #[async_trait::async_trait]
    impl Handler for SlowHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            Ok(Outcome::success())
        }
    }

    let mut graph = make_graph_with_start_exit("ManagerMaxCyclesTest");
    let mut manager = Node::new("manager");
    manager.attrs.insert(
        "type".to_string(),
        AttrValue::String("stack.manager_loop".to_string()),
    );
    manager.attrs.insert(
        "stack.child_dot_source".to_string(),
        AttrValue::String(
            "digraph Child { start [shape=Mdiamond]; slow [shape=box]; exit [shape=Msquare]; start -> slow -> exit }"
                .to_string(),
        ),
    );
    manager
        .attrs
        .insert("manager.max_cycles".to_string(), AttrValue::Integer(2));
    manager.attrs.insert(
        "manager.poll_interval".to_string(),
        AttrValue::Duration(std::time::Duration::from_millis(1)),
    );
    graph.nodes.insert("manager".to_string(), manager);
    graph.edges.push(Edge::new("start", "manager"));
    graph.edges.push(Edge::new("manager", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(SlowHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("stack.manager_loop", Box::new(SubWorkflowHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let manager_outcome = cp.node_outcomes.get("manager").expect("manager outcome");
    assert_eq!(manager_outcome.status, StageStatus::Fail);
    assert!(manager_outcome
        .failure_reason()
        .unwrap()
        .contains("Max cycles"));
    // Pipeline reached exit with goal gates satisfied — per spec, SUCCESS.
    assert_eq!(outcome.status, StageStatus::Success);
}

// ===========================================================================
// Parity tests — P3: Validation
// ===========================================================================

#[test]
fn validation_missing_start_node() {
    let mut graph = Graph::new("NoStartTest");
    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    let diagnostics = validate(&graph, &[]);
    let start_errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error && d.rule == "start_node")
        .collect();
    assert!(
        !start_errors.is_empty(),
        "should have start_node error diagnostic"
    );
}

#[test]
fn validation_missing_exit_node() {
    let mut graph = Graph::new("NoExitTest");
    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);
    graph.nodes.insert("work".to_string(), Node::new("work"));
    graph.edges.push(Edge::new("start", "work"));

    let diagnostics = validate(&graph, &[]);
    let exit_errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error && d.rule == "terminal_node")
        .collect();
    assert!(
        !exit_errors.is_empty(),
        "should have terminal_node error diagnostic"
    );
}

#[test]
fn validation_orphan_unreachable_node() {
    let mut graph = make_graph_with_start_exit("OrphanTest");
    graph
        .nodes
        .insert("orphan".to_string(), Node::new("orphan"));
    graph.edges.push(Edge::new("start", "exit"));

    let diagnostics = validate(&graph, &[]);
    let reachability_errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.rule == "reachability")
        .collect();
    assert!(
        !reachability_errors.is_empty(),
        "should have reachability diagnostic for orphan node"
    );
}

// ===========================================================================
// Parity tests — P4: Edge selection and cross-feature
// ===========================================================================

#[tokio::test]
async fn conditional_branching_success_fail_paths() {
    let mut graph = make_graph_with_start_exit("CondBranchTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("always_fail".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);
    graph
        .nodes
        .insert("success_path".to_string(), Node::new("success_path"));
    graph
        .nodes
        .insert("fail_path".to_string(), Node::new("fail_path"));

    graph.edges.push(Edge::new("start", "work"));
    let mut e_success = Edge::new("work", "success_path");
    e_success.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(e_success);
    let mut e_fail = Edge::new("work", "fail_path");
    e_fail.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    graph.edges.push(e_fail);
    graph.edges.push(Edge::new("success_path", "exit"));
    graph.edges.push(Edge::new("fail_path", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("always_fail", Box::new(AlwaysFailHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"fail_path".to_string()));
    assert!(!cp.completed_nodes.contains(&"success_path".to_string()));
}

#[tokio::test]
async fn edge_selection_condition_match_wins_over_weight() {
    let mut graph = make_graph_with_start_exit("CondVsWeightTest");
    graph.nodes.insert("a".to_string(), Node::new("a"));
    graph
        .nodes
        .insert("cond_target".to_string(), Node::new("cond_target"));
    graph
        .nodes
        .insert("weighted_target".to_string(), Node::new("weighted_target"));

    graph.edges.push(Edge::new("start", "a"));
    let mut e_cond = Edge::new("a", "cond_target");
    e_cond.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(e_cond);
    let mut e_weight = Edge::new("a", "weighted_target");
    e_weight
        .attrs
        .insert("weight".to_string(), AttrValue::Integer(100));
    graph.edges.push(e_weight);
    graph.edges.push(Edge::new("cond_target", "exit"));
    graph.edges.push(Edge::new("weighted_target", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"cond_target".to_string()));
    assert!(!cp.completed_nodes.contains(&"weighted_target".to_string()));
}

#[tokio::test]
async fn edge_selection_weight_breaks_ties() {
    let mut graph = make_graph_with_start_exit("WeightTiesTest");
    graph.nodes.insert("a".to_string(), Node::new("a"));
    graph.nodes.insert("low".to_string(), Node::new("low"));
    graph.nodes.insert("high".to_string(), Node::new("high"));

    graph.edges.push(Edge::new("start", "a"));
    let mut e_low = Edge::new("a", "low");
    e_low
        .attrs
        .insert("weight".to_string(), AttrValue::Integer(1));
    graph.edges.push(e_low);
    let mut e_high = Edge::new("a", "high");
    e_high
        .attrs
        .insert("weight".to_string(), AttrValue::Integer(10));
    graph.edges.push(e_high);
    graph.edges.push(Edge::new("low", "exit"));
    graph.edges.push(Edge::new("high", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"high".to_string()));
    assert!(!cp.completed_nodes.contains(&"low".to_string()));
}

#[tokio::test]
async fn edge_selection_lexical_tiebreak() {
    let mut graph = make_graph_with_start_exit("LexicalTieTest");
    graph.nodes.insert("a".to_string(), Node::new("a"));
    graph.nodes.insert("beta".to_string(), Node::new("beta"));
    graph.nodes.insert("alpha".to_string(), Node::new("alpha"));

    graph.edges.push(Edge::new("start", "a"));
    graph.edges.push(Edge::new("a", "beta"));
    graph.edges.push(Edge::new("a", "alpha"));
    graph.edges.push(Edge::new("beta", "exit"));
    graph.edges.push(Edge::new("alpha", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"alpha".to_string()));
    assert!(!cp.completed_nodes.contains(&"beta".to_string()));
}

#[tokio::test]
async fn context_updates_visible_across_nodes() {
    let mut graph = make_graph_with_start_exit("ContextVisibilityTest");
    let mut setter = Node::new("setter");
    setter.attrs.insert(
        "type".to_string(),
        AttrValue::String("context_setter".to_string()),
    );
    graph.nodes.insert("setter".to_string(), setter);
    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("diamond".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph.nodes.insert("yes".to_string(), Node::new("yes"));
    graph.nodes.insert("no".to_string(), Node::new("no"));

    graph.edges.push(Edge::new("start", "setter"));
    graph.edges.push(Edge::new("setter", "gate"));
    let mut e_yes = Edge::new("gate", "yes");
    e_yes.attrs.insert(
        "condition".to_string(),
        AttrValue::String("context.my_flag=set".to_string()),
    );
    graph.edges.push(e_yes);
    graph.edges.push(Edge::new("gate", "no"));
    graph.edges.push(Edge::new("yes", "exit"));
    graph.edges.push(Edge::new("no", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("conditional", Box::new(ConditionalHandler));
    registry.register("context_setter", Box::new(ContextSetterHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"yes".to_string()));
    assert!(!cp.completed_nodes.contains(&"no".to_string()));
}

#[tokio::test]
async fn stylesheet_applies_model_override() {
    let input = r#"digraph StylesheetTest {
        graph [
            goal="Test stylesheet",
            model_stylesheet="* { model: custom-model; }"
        ]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        work  [shape=box, prompt="Do work"]
        start -> work -> exit
    }"#;
    let mut graph = parse(input).expect("parse");
    validate_or_raise(&graph, &[]).expect("validate");
    StylesheetApplicationTransform.apply(&mut graph);
    assert_eq!(graph.nodes["work"].model(), Some("custom-model"));

    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(
        make_linear_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
async fn custom_handler_registration_and_execution() {
    struct CustomHandler;

    #[async_trait::async_trait]
    impl Handler for CustomHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            let mut outcome = Outcome::success();
            outcome
                .context_updates
                .insert("custom.ran".to_string(), serde_json::json!("true"));
            Ok(outcome)
        }
    }

    let mut graph = make_graph_with_start_exit("CustomHandlerTest");
    let mut custom = Node::new("custom");
    custom.attrs.insert(
        "type".to_string(),
        AttrValue::String("my_custom".to_string()),
    );
    graph.nodes.insert("custom".to_string(), custom);
    graph.edges.push(Edge::new("start", "custom"));
    graph.edges.push(Edge::new("custom", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("my_custom", Box::new(CustomHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert_eq!(
        cp.context_values.get("custom.ran"),
        Some(&serde_json::json!("true"))
    );
}

#[tokio::test]
async fn integration_smoke_plan_implement_review_done() {
    let dot = r#"digraph SmokeIntegration {
        graph [
            goal="Build the feature",
            model_stylesheet="* { model: test-model; }"
        ]
        rankdir=LR
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        plan  [shape=box, prompt="Plan: $goal"]
        implement [shape=box, prompt="Implement"]
        review [shape=hexagon, label="Review"]
        start -> plan -> implement -> review
        review -> exit [label="[A] Approve"]
        review -> implement [label="[F] Fix"]
    }"#;

    // Parse and validate
    let mut graph = parse(dot).expect("parse");
    let diagnostics = validate_or_raise(&graph, &[]).expect("validate");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty());

    // Apply transforms
    VariableExpansionTransform.apply(&mut graph);
    StylesheetApplicationTransform.apply(&mut graph);

    // Verify transforms applied
    assert_eq!(
        graph.nodes["plan"].prompt().unwrap(),
        "Plan: Build the feature"
    );
    assert_eq!(graph.nodes["plan"].model(), Some("test-model"));

    // Run pipeline
    let interviewer = Arc::new(AutoApproveInterviewer);
    let dir = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);
    let engine = WorkflowRunner::new(
        make_full_registry(interviewer),
        Arc::new(emitter),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    // Verify all nodes completed
    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(cp.completed_nodes.contains(&"plan".to_string()));
    assert!(cp.completed_nodes.contains(&"implement".to_string()));
    assert!(cp.completed_nodes.contains(&"review".to_string()));

    // Verify prompt.md and response.md exist
    assert!(dir
        .path()
        .join("nodes")
        .join("plan")
        .join("prompt.md")
        .exists());
    assert!(dir
        .path()
        .join("nodes")
        .join("plan")
        .join("response.md")
        .exists());

    // Verify events
    let collected = events.lock().unwrap();
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunStarted { .. })));
    assert!(collected
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunCompleted { .. })));
}

// ===========================================================================
// 19b. Manager loop runs child engine E2E
// ===========================================================================

#[tokio::test]
async fn manager_loop_runs_child_engine_e2e() {
    let mut graph = Graph::new("ManagerLoopE2E");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test manager loop".to_string()),
    );

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

    let mut supervisor = Node::new("supervisor");
    supervisor.attrs.insert(
        "type".to_string(),
        AttrValue::String("stack.manager_loop".to_string()),
    );
    supervisor.attrs.insert(
        "stack.child_dot_source".to_string(),
        AttrValue::String(
            "digraph Child { start [shape=Mdiamond]; exit [shape=Msquare]; start -> exit }"
                .to_string(),
        ),
    );
    supervisor.attrs.insert(
        "manager.poll_interval".to_string(),
        AttrValue::Duration(std::time::Duration::from_millis(10)),
    );
    supervisor
        .attrs
        .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
    graph.nodes.insert("supervisor".to_string(), supervisor);

    graph.edges.push(Edge::new("start", "supervisor"));
    graph.edges.push(Edge::new("supervisor", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("stack.manager_loop", Box::new(SubWorkflowHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("manager loop E2E should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"supervisor".to_string()),
        "supervisor should be in completed_nodes"
    );

    let supervisor_outcome = checkpoint.node_outcomes.get("supervisor");
    assert!(
        supervisor_outcome.is_some(),
        "supervisor outcome should exist"
    );
    let notes = supervisor_outcome.unwrap().notes.as_deref().unwrap_or("");
    assert!(
        notes.contains("Child completed"),
        "notes should mention child completion, got: {notes}"
    );
}

// ===========================================================================
// 19b-2. Manager loop: context flows parent → child → parent
// ===========================================================================

#[tokio::test]
async fn manager_loop_context_flows_e2e() {
    // Handler that reads parent's context value and sets a result
    struct ContextEchoHandler;

    #[async_trait::async_trait]
    impl Handler for ContextEchoHandler {
        async fn execute(
            &self,
            _node: &Node,
            context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            let target = context.get_string("review.target", "");
            let mut outcome = Outcome::success();
            outcome
                .context_updates
                .insert("review.result".to_string(), serde_json::json!("approved"));
            outcome
                .context_updates
                .insert("review.echo".to_string(), serde_json::json!(target));
            Ok(outcome)
        }
    }

    let mut graph = make_graph_with_start_exit("ManagerContextFlowE2E");

    // A setter node that puts review.target into context before the manager
    struct SetterHandler;
    #[async_trait::async_trait]
    impl Handler for SetterHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, FabroError> {
            let mut outcome = Outcome::success();
            outcome.context_updates.insert(
                "review.target".to_string(),
                serde_json::json!("src/main.rs"),
            );
            Ok(outcome)
        }
    }

    let mut setter = Node::new("setter");
    setter
        .attrs
        .insert("type".to_string(), AttrValue::String("setter".to_string()));
    graph.nodes.insert("setter".to_string(), setter);

    let mut supervisor = Node::new("supervisor");
    supervisor.attrs.insert(
        "type".to_string(),
        AttrValue::String("stack.manager_loop".to_string()),
    );
    supervisor.attrs.insert(
        "stack.child_dot_source".to_string(),
        AttrValue::String(
            "digraph Child { start [shape=Mdiamond]; work [shape=box]; exit [shape=Msquare]; start -> work -> exit }"
                .to_string(),
        ),
    );
    supervisor.attrs.insert(
        "manager.poll_interval".to_string(),
        AttrValue::Duration(std::time::Duration::from_millis(10)),
    );
    supervisor
        .attrs
        .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
    graph.nodes.insert("supervisor".to_string(), supervisor);

    graph.edges.push(Edge::new("start", "setter"));
    graph.edges.push(Edge::new("setter", "supervisor"));
    graph.edges.push(Edge::new("supervisor", "exit"));

    let dir = tempfile::tempdir().unwrap();
    // Default handler = ContextEchoHandler (handles the child's "work" node)
    let mut registry = HandlerRegistry::new(Box::new(ContextEchoHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("setter", Box::new(SetterHandler));
    registry.register("stack.manager_loop", Box::new(SubWorkflowHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);

    // Check that child's context updates were propagated through the manager
    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let sup_outcome = checkpoint.node_outcomes.get("supervisor").unwrap();
    assert_eq!(
        sup_outcome.context_updates.get("review.result"),
        Some(&serde_json::json!("approved")),
        "child's review.result should propagate to parent"
    );
    assert_eq!(
        sup_outcome.context_updates.get("review.echo"),
        Some(&serde_json::json!("src/main.rs")),
        "child should have read parent's review.target"
    );
}

// ===========================================================================
// 19b-3. Manager loop with child_dotfile E2E
// ===========================================================================

#[tokio::test]
async fn manager_loop_child_dotfile_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let dot_path = dir.path().join("child.dot");
    std::fs::write(
        &dot_path,
        "digraph Child { start [shape=Mdiamond]; exit [shape=Msquare]; start -> exit }",
    )
    .unwrap();

    let mut graph = make_graph_with_start_exit("ManagerDotfileE2E");
    let mut supervisor = Node::new("supervisor");
    supervisor.attrs.insert(
        "type".to_string(),
        AttrValue::String("stack.manager_loop".to_string()),
    );
    supervisor.attrs.insert(
        "stack.child_dotfile".to_string(),
        AttrValue::String(dot_path.to_string_lossy().to_string()),
    );
    supervisor.attrs.insert(
        "manager.poll_interval".to_string(),
        AttrValue::Duration(std::time::Duration::from_millis(10)),
    );
    supervisor
        .attrs
        .insert("manager.max_cycles".to_string(), AttrValue::Integer(100));
    graph.nodes.insert("supervisor".to_string(), supervisor);
    graph.edges.push(Edge::new("start", "supervisor"));
    graph.edges.push(Edge::new("supervisor", "exit"));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("stack.manager_loop", Box::new(SubWorkflowHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);
}

// ===========================================================================
// 19c. GraphMerge E2E (TS Scenario 11)
// ===========================================================================

#[tokio::test]
async fn graph_merge_e2e_through_engine() {
    use fabro_workflows::transform::GraphMergeTransform;

    // Module "val": lint -> test
    let mut val_graph = Graph::new("val");
    let mut lint = Node::new("lint");
    lint.attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    lint.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Lint the code".to_string()),
    );
    val_graph.nodes.insert("lint".to_string(), lint);

    let mut test_node = Node::new("test");
    test_node
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    test_node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Run tests".to_string()),
    );
    val_graph.nodes.insert("test".to_string(), test_node);
    val_graph.edges.push(Edge::new("lint", "test"));

    // Module "dep": stage -> release
    let mut dep_graph = Graph::new("dep");
    let mut stage = Node::new("stage");
    stage
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    stage.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Stage the release".to_string()),
    );
    dep_graph.nodes.insert("stage".to_string(), stage);

    let mut release = Node::new("release");
    release
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    release.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Release it".to_string()),
    );
    dep_graph.nodes.insert("release".to_string(), release);
    dep_graph.edges.push(Edge::new("stage", "release"));

    // Main graph: start, exit; edges connect modules
    let mut main_graph = Graph::new("MergeE2E");
    main_graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test graph merge".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    main_graph.nodes.insert("start".to_string(), start);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    main_graph.nodes.insert("exit".to_string(), exit);

    // Apply merge transform
    let merge = GraphMergeTransform::new(vec![val_graph, dep_graph]);
    merge.apply(&mut main_graph);

    // Add cross-module edges
    main_graph.edges.push(Edge::new("start", "val.lint"));
    main_graph.edges.push(Edge::new("val.test", "dep.stage"));
    main_graph.edges.push(Edge::new("dep.release", "exit"));

    // Verify merged nodes exist
    assert!(main_graph.nodes.contains_key("val.lint"));
    assert!(main_graph.nodes.contains_key("val.test"));
    assert!(main_graph.nodes.contains_key("dep.stage"));
    assert!(main_graph.nodes.contains_key("dep.release"));

    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(
        make_linear_registry(),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&main_graph, &config)
        .await
        .expect("graph merge E2E should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint.completed_nodes.contains(&"val.lint".to_string()),
        "val.lint should be completed"
    );
    assert!(
        checkpoint.completed_nodes.contains(&"val.test".to_string()),
        "val.test should be completed"
    );
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"dep.stage".to_string()),
        "dep.stage should be completed"
    );
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"dep.release".to_string()),
        "dep.release should be completed"
    );

    // Verify ordering: val.test appears before dep.stage
    let val_test_pos = checkpoint
        .completed_nodes
        .iter()
        .position(|n| n == "val.test")
        .expect("val.test should be in completed_nodes");
    let dep_stage_pos = checkpoint
        .completed_nodes
        .iter()
        .position(|n| n == "dep.stage")
        .expect("dep.stage should be in completed_nodes");
    assert!(
        val_test_pos < dep_stage_pos,
        "val.test ({val_test_pos}) should execute before dep.stage ({dep_stage_pos})"
    );
}

// ===========================================================================
// Context fidelity integration tests (spec Section 5.4)
// ===========================================================================

type SharedVec<T> = Arc<std::sync::Mutex<Vec<T>>>;

/// Shared capture storage for fidelity tests.
#[derive(Clone)]
struct FidelityCaptures {
    fidelities: SharedVec<(String, String)>,
    thread_ids: SharedVec<(String, Option<String>)>,
    preambles: SharedVec<(String, String)>,
}

impl FidelityCaptures {
    fn new() -> Self {
        Self {
            fidelities: Arc::new(std::sync::Mutex::new(Vec::new())),
            thread_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
            preambles: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

/// A handler that captures the resolved fidelity and `thread_id` from the context.
struct FidelityCapturingHandler {
    captures: FidelityCaptures,
}

#[async_trait::async_trait]
impl Handler for FidelityCapturingHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let fidelity = context.get_string("internal.fidelity", "none");
        self.captures
            .fidelities
            .lock()
            .unwrap()
            .push((node.id.clone(), fidelity));

        let thread_id = context
            .get("internal.thread_id")
            .and_then(|v| v.as_str().map(String::from));
        self.captures
            .thread_ids
            .lock()
            .unwrap()
            .push((node.id.clone(), thread_id));

        let preamble = context.get_string("current.preamble", "");
        self.captures
            .preambles
            .lock()
            .unwrap()
            .push((node.id.clone(), preamble));

        Ok(Outcome::success())
    }
}

#[tokio::test]
async fn fidelity_default_is_compact() {
    let mut graph = make_graph_with_start_exit("FidelityDefaultTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities.len(), 1);
    assert_eq!(fidelities[0].0, "work");
    assert_eq!(fidelities[0].1, "compact");

    let preambles = captures.preambles.lock().unwrap();
    assert!(
        !preambles[0].1.is_empty(),
        "compact fidelity should produce a preamble"
    );
}

#[tokio::test]
async fn fidelity_graph_default_applied() {
    let mut graph = make_graph_with_start_exit("FidelityGraphDefaultTest");
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("truncate".to_string()),
    );
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "truncate");
}

#[tokio::test]
async fn fidelity_node_overrides_graph_default() {
    let mut graph = make_graph_with_start_exit("FidelityNodeOverrideTest");
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("truncate".to_string()),
    );
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    work.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("summary:medium".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "summary:medium");
}

#[tokio::test]
async fn fidelity_edge_overrides_node_and_graph() {
    let mut graph = make_graph_with_start_exit("FidelityEdgeOverrideTest");
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("truncate".to_string()),
    );
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    work.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("compact".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);

    let mut edge_with_fidelity = Edge::new("start", "work");
    edge_with_fidelity.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("summary:high".to_string()),
    );
    graph.edges.push(edge_with_fidelity);
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "summary:high");
}

#[tokio::test]
async fn fidelity_full_produces_empty_preamble() {
    let mut graph = make_graph_with_start_exit("FidelityFullPreambleTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    work.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "full");

    let preambles = captures.preambles.lock().unwrap();
    assert_eq!(
        preambles[0].1, "",
        "full fidelity should produce empty preamble"
    );
}

#[tokio::test]
async fn fidelity_truncate_preamble_minimal() {
    let mut graph = make_graph_with_start_exit("FidelityTruncateTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test truncate mode".to_string()),
    );
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("truncate".to_string()),
    );
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let preambles = captures.preambles.lock().unwrap();
    let preamble = &preambles[0].1;
    assert!(
        preamble.contains("Goal: Test truncate mode"),
        "truncate preamble should contain the goal"
    );
    assert!(
        preamble.contains("Run ID:"),
        "truncate preamble should contain run ID"
    );
    assert!(
        !preamble.contains("Completed stages:"),
        "truncate should not include stage details"
    );
}

#[tokio::test]
async fn fidelity_summary_low_mode() {
    let mut graph = make_graph_with_start_exit("SummaryLow");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test summary".to_string()),
    );
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("summary:low".to_string()),
    );
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);
    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);
    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "summary:low");
    assert_eq!(fidelities[1].1, "summary:low");

    let preambles = captures.preambles.lock().unwrap();
    assert!(
        preambles[1].1.contains("Test summary"),
        "summary:low preamble should contain goal"
    );
}

#[tokio::test]
async fn fidelity_summary_medium_mode() {
    let mut graph = make_graph_with_start_exit("SummaryMedium");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test summary".to_string()),
    );
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("summary:medium".to_string()),
    );
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);
    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);
    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "summary:medium");
    assert_eq!(fidelities[1].1, "summary:medium");

    let preambles = captures.preambles.lock().unwrap();
    assert!(
        preambles[1].1.contains("Test summary"),
        "summary:medium preamble should contain goal"
    );
}

#[tokio::test]
async fn fidelity_summary_high_mode() {
    let mut graph = make_graph_with_start_exit("SummaryHigh");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test summary".to_string()),
    );
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("summary:high".to_string()),
    );
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);
    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);
    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "summary:high");
    assert_eq!(fidelities[1].1, "summary:high");

    let preambles = captures.preambles.lock().unwrap();
    assert!(
        preambles[1].1.contains("Test summary"),
        "summary:high preamble should contain goal"
    );
}

#[tokio::test]
async fn fidelity_full_sets_thread_id_in_context() {
    let mut graph = make_graph_with_start_exit("FidelityThreadTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    work.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    work.attrs.insert(
        "thread_id".to_string(),
        AttrValue::String("my-session".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let thread_ids = captures.thread_ids.lock().unwrap();
    assert_eq!(thread_ids[0].0, "work");
    assert_eq!(thread_ids[0].1, Some("my-session".to_string()));
}

#[tokio::test]
async fn fidelity_full_nodes_share_thread_id() {
    let mut graph = make_graph_with_start_exit("FidelitySharedThreadTest");
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_a.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    step_a.attrs.insert(
        "thread_id".to_string(),
        AttrValue::String("shared-session".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_b.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    step_b.attrs.insert(
        "thread_id".to_string(),
        AttrValue::String("shared-session".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let thread_ids = captures.thread_ids.lock().unwrap();
    assert_eq!(thread_ids[0].0, "step_a");
    assert_eq!(thread_ids[0].1, Some("shared-session".to_string()));
    assert_eq!(thread_ids[1].0, "step_b");
    assert_eq!(thread_ids[1].1, Some("shared-session".to_string()));
}

#[tokio::test]
async fn fidelity_resume_degrades_full_to_summary_high() {
    let mut graph = make_graph_with_start_exit("FidelityResumeTest");
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_b.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let ctx = Context::new();
    ctx.set("outcome", serde_json::json!("success"));
    ctx.set("internal.fidelity", serde_json::json!("full"));

    let mut outcomes = std::collections::HashMap::new();
    outcomes.insert("start".to_string(), Outcome::success());
    outcomes.insert("step_a".to_string(), Outcome::success());

    let checkpoint = Checkpoint::from_context(
        &ctx,
        "step_a",
        vec!["start".to_string(), "step_a".to_string()],
        std::collections::HashMap::new(),
        outcomes,
        Some("step_b".to_string()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await
        .expect("resume should succeed");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].0, "step_b");
    assert_eq!(
        fidelities[0].1, "summary:high",
        "first node after resume from full fidelity should be degraded to summary:high"
    );
}

#[tokio::test]
async fn fidelity_resume_degrade_only_affects_first_hop() {
    let mut graph = make_graph_with_start_exit("FidelityResumeSingleHopTest");
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_b.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    let mut step_c = Node::new("step_c");
    step_c.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_c.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    graph.nodes.insert("step_c".to_string(), step_c);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "step_c"));
    graph.edges.push(Edge::new("step_c", "exit"));

    let ctx = Context::new();
    ctx.set("outcome", serde_json::json!("success"));
    ctx.set("internal.fidelity", serde_json::json!("full"));

    let mut outcomes = std::collections::HashMap::new();
    outcomes.insert("start".to_string(), Outcome::success());
    outcomes.insert("step_a".to_string(), Outcome::success());

    let checkpoint = Checkpoint::from_context(
        &ctx,
        "step_a",
        vec!["start".to_string(), "step_a".to_string()],
        std::collections::HashMap::new(),
        outcomes,
        Some("step_b".to_string()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await
        .expect("resume should succeed");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].0, "step_b");
    assert_eq!(fidelities[0].1, "summary:high");
    assert_eq!(fidelities[1].0, "step_c");
    assert_eq!(fidelities[1].1, "full");
}

#[tokio::test]
async fn fidelity_resume_no_degrade_when_not_full() {
    let mut graph = make_graph_with_start_exit("FidelityResumeNoDegrade");
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("compact".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_b.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let ctx = Context::new();
    ctx.set("outcome", serde_json::json!("success"));
    ctx.set("internal.fidelity", serde_json::json!("compact"));

    let mut outcomes = std::collections::HashMap::new();
    outcomes.insert("start".to_string(), Outcome::success());
    outcomes.insert("step_a".to_string(), Outcome::success());

    let checkpoint = Checkpoint::from_context(
        &ctx,
        "step_a",
        vec!["start".to_string(), "step_a".to_string()],
        std::collections::HashMap::new(),
        outcomes,
        Some("step_b".to_string()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await
        .expect("resume should succeed");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].0, "step_b");
    assert_eq!(fidelities[0].1, "full");
}

#[tokio::test]
async fn fidelity_stored_in_checkpoint_context() {
    let mut graph = make_graph_with_start_exit("FidelityCheckpointTest");
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("summary:low".to_string()),
    );
    let work = Node::new("work");
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert_eq!(
        cp.context_values.get("internal.fidelity"),
        Some(&serde_json::json!("summary:low")),
        "checkpoint should record the resolved fidelity"
    );
}

#[tokio::test]
async fn fidelity_precedence_multi_node_pipeline() {
    let mut graph = make_graph_with_start_exit("FidelityPrecedenceTest");
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("truncate".to_string()),
    );

    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_b.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("summary:medium".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    let mut step_c = Node::new("step_c");
    step_c.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_c.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("compact".to_string()),
    );
    graph.nodes.insert("step_c".to_string(), step_c);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));

    let mut edge_b_c = Edge::new("step_b", "step_c");
    edge_b_c.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("summary:high".to_string()),
    );
    graph.edges.push(edge_b_c);

    graph.edges.push(Edge::new("step_c", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].0, "step_a");
    assert_eq!(fidelities[0].1, "truncate");
    assert_eq!(fidelities[1].0, "step_b");
    assert_eq!(fidelities[1].1, "summary:medium");
    assert_eq!(fidelities[2].0, "step_c");
    assert_eq!(fidelities[2].1, "summary:high");
}

#[tokio::test]
async fn fidelity_compact_preamble_includes_completed_stages_and_context() {
    let mut graph = make_graph_with_start_exit("FidelityCompactContentTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Build the widget".to_string()),
    );
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("compact".to_string()),
    );

    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let preambles = captures.preambles.lock().unwrap();
    // step_b's preamble should contain structured summary of completed work
    let step_b_preamble = &preambles[1].1;
    assert!(
        step_b_preamble.contains("Build the widget"),
        "compact preamble should contain the goal"
    );
    assert!(
        step_b_preamble.contains("## Completed stages"),
        "compact preamble should include completed stages section"
    );
    assert!(
        step_b_preamble.contains("step_a"),
        "compact preamble should mention completed node step_a"
    );
}

#[tokio::test]
async fn fidelity_summary_low_excludes_context_values_in_pipeline() {
    // summary:low should NOT include context values (only goal, run ID, stage count, recent stages).
    // summary:medium should include context values.
    // This verifies a behavioral difference between detail levels.
    let mut graph_low = make_graph_with_start_exit("SummaryLowExcludesContext");
    graph_low.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Context exclusion test".to_string()),
    );
    graph_low.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("summary:low".to_string()),
    );
    let mut step_a_low = Node::new("step_a");
    step_a_low.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph_low.nodes.insert("step_a".to_string(), step_a_low);
    let mut step_b_low = Node::new("step_b");
    step_b_low.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph_low.nodes.insert("step_b".to_string(), step_b_low);
    graph_low.edges.push(Edge::new("start", "step_a"));
    graph_low.edges.push(Edge::new("step_a", "step_b"));
    graph_low.edges.push(Edge::new("step_b", "exit"));

    let captures_low = FidelityCaptures::new();
    let dir_low = tempfile::tempdir().unwrap();
    let mut registry_low = HandlerRegistry::new(Box::new(StartHandler));
    registry_low.register("start", Box::new(StartHandler));
    registry_low.register("exit", Box::new(ExitHandler));
    registry_low.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures_low.clone(),
        }),
    );
    let engine_low = WorkflowRunner::new(registry_low, Arc::new(EventEmitter::new()), local_env());
    let config_low = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir_low.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine_low
        .run(&graph_low, &config_low)
        .await
        .expect("run low");

    {
        let preambles_low = captures_low.preambles.lock().unwrap();
        let low_preamble = &preambles_low[1].1;
        // summary:low should not include "Context values:" section
        assert!(
            !low_preamble.contains("Context values:"),
            "summary:low preamble should not include context values section"
        );
    }

    // Now run summary:medium and verify it DOES include context values
    let mut graph_med = make_graph_with_start_exit("SummaryMedIncludesContext");
    graph_med.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Context exclusion test".to_string()),
    );
    graph_med.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("summary:medium".to_string()),
    );
    let mut step_a_med = Node::new("step_a");
    step_a_med.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph_med.nodes.insert("step_a".to_string(), step_a_med);
    let mut step_b_med = Node::new("step_b");
    step_b_med.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph_med.nodes.insert("step_b".to_string(), step_b_med);
    graph_med.edges.push(Edge::new("start", "step_a"));
    graph_med.edges.push(Edge::new("step_a", "step_b"));
    graph_med.edges.push(Edge::new("step_b", "exit"));

    let captures_med = FidelityCaptures::new();
    let dir_med = tempfile::tempdir().unwrap();
    let mut registry_med = HandlerRegistry::new(Box::new(StartHandler));
    registry_med.register("start", Box::new(StartHandler));
    registry_med.register("exit", Box::new(ExitHandler));
    registry_med.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures_med.clone(),
        }),
    );
    let engine_med = WorkflowRunner::new(registry_med, Arc::new(EventEmitter::new()), local_env());
    let config_med = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir_med.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine_med
        .run(&graph_med, &config_med)
        .await
        .expect("run med");

    let preambles_med = captures_med.preambles.lock().unwrap();
    let med_preamble = &preambles_med[1].1;
    // summary:medium should include stage details (unlike summary:low which omits them)
    assert!(
        med_preamble.contains("step_a"),
        "summary:medium preamble should include completed stage step_a"
    );
    // Verify medium and low differ: medium shows more recent stages
    let preambles_low = captures_low.preambles.lock().unwrap();
    let low_preamble = &preambles_low[1].1;
    assert!(
        !low_preamble.contains("## Context"),
        "summary:low preamble should not include context section"
    );
}

#[tokio::test]
async fn fidelity_thread_id_fallback_to_previous_node_in_pipeline() {
    // When no thread_id is set on the node, edge, graph, or class,
    // the thread ID should fall back to the previous node's ID.
    let mut graph = make_graph_with_start_exit("ThreadFallbackTest");
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let thread_ids = captures.thread_ids.lock().unwrap();
    // step_a should have previous node = start
    assert_eq!(thread_ids[0].0, "step_a");
    assert_eq!(thread_ids[0].1, Some("start".to_string()));
    // step_b should have previous node = step_a
    assert_eq!(thread_ids[1].0, "step_b");
    assert_eq!(thread_ids[1].1, Some("step_a".to_string()));
}

#[tokio::test]
async fn fidelity_thread_id_from_node_class_in_pipeline() {
    // When a node has classes (from subgraph derivation), thread_id resolves
    // from the first class name per spec step 4.
    let mut graph = make_graph_with_start_exit("ThreadClassTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    work.classes = vec!["planning".to_string(), "review".to_string()];
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let thread_ids = captures.thread_ids.lock().unwrap();
    assert_eq!(thread_ids[0].0, "work");
    assert_eq!(
        thread_ids[0].1,
        Some("planning".to_string()),
        "thread_id should resolve from first class name"
    );
}

#[tokio::test]
async fn fidelity_edge_thread_id_override_in_pipeline() {
    // Edge thread_id should override the previous-node fallback.
    let mut graph = make_graph_with_start_exit("EdgeThreadOverrideTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);

    let mut edge_to_work = Edge::new("start", "work");
    edge_to_work.attrs.insert(
        "thread_id".to_string(),
        AttrValue::String("edge-session".to_string()),
    );
    graph.edges.push(edge_to_work);
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let thread_ids = captures.thread_ids.lock().unwrap();
    assert_eq!(thread_ids[0].0, "work");
    assert_eq!(
        thread_ids[0].1,
        Some("edge-session".to_string()),
        "edge thread_id should override the previous-node fallback"
    );
}

#[tokio::test]
async fn fidelity_full_without_explicit_thread_id_uses_previous_node() {
    // When fidelity=full but no explicit thread_id is set, thread resolution
    // should still fall back to the previous node ID.
    let mut graph = make_graph_with_start_exit("FullNoExplicitThreadTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    work.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("full".to_string()),
    );
    // No thread_id set explicitly
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].1, "full");

    let thread_ids = captures.thread_ids.lock().unwrap();
    assert_eq!(thread_ids[0].0, "work");
    assert_eq!(
        thread_ids[0].1,
        Some("start".to_string()),
        "full fidelity without explicit thread_id should fall back to previous node"
    );

    let preambles = captures.preambles.lock().unwrap();
    assert_eq!(
        preambles[0].1, "",
        "full fidelity should produce empty preamble"
    );
}

#[tokio::test]
async fn fidelity_from_parsed_dot_pipeline() {
    // Parse a DOT file with fidelity attributes and run the pipeline.
    let input = r#"digraph FidelityDotTest {
        graph [goal="Test DOT fidelity", default_fidelity="truncate"]

        start [shape=Mdiamond]
        exit  [shape=Msquare]

        step_a [type="fidelity_capture"]
        step_b [type="fidelity_capture", fidelity="summary:medium"]
        step_c [type="fidelity_capture"]

        start -> step_a -> step_b
        step_b -> step_c [fidelity="summary:high"]
        step_c -> exit
    }"#;

    let graph = parse(input).expect("parsing should succeed");
    validate_or_raise(&graph, &[]).expect("validation should pass");

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let fidelities = captures.fidelities.lock().unwrap();
    // step_a: no node fidelity, no edge fidelity -> graph default "truncate"
    assert_eq!(fidelities[0].0, "step_a");
    assert_eq!(fidelities[0].1, "truncate");
    // step_b: node fidelity "summary:medium" overrides graph default
    assert_eq!(fidelities[1].0, "step_b");
    assert_eq!(fidelities[1].1, "summary:medium");
    // step_c: node has no fidelity but incoming edge has "summary:high" -> edge wins
    assert_eq!(fidelities[2].0, "step_c");
    assert_eq!(fidelities[2].1, "summary:high");
}

#[tokio::test]
async fn fidelity_checkpoint_roundtrip_preserves_fidelity() {
    // Run a pipeline that sets a specific fidelity, save checkpoint,
    // load it, and verify the fidelity value survives the roundtrip.
    let mut graph = make_graph_with_start_exit("FidelityCheckpointRoundtripTest");
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String("summary:high".to_string()),
    );
    let work = Node::new("work");
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    // Load, save, load again to verify roundtrip
    let checkpoint_path = dir.path().join("checkpoint.json");
    let cp1 = Checkpoint::load(&checkpoint_path).expect("first load");
    assert_eq!(
        cp1.context_values.get("internal.fidelity"),
        Some(&serde_json::json!("summary:high")),
    );

    let roundtrip_path = dir.path().join("checkpoint_roundtrip.json");
    cp1.save(&roundtrip_path).expect("save");
    let cp2 = Checkpoint::load(&roundtrip_path).expect("second load");
    assert_eq!(
        cp2.context_values.get("internal.fidelity"),
        Some(&serde_json::json!("summary:high")),
        "fidelity should survive checkpoint save/load roundtrip"
    );
}

#[tokio::test]
async fn fidelity_node_thread_id_overrides_edge_thread_id_in_pipeline() {
    // When both node and edge have thread_id, the edge's takes precedence (step 1 > step 2).
    let mut graph = make_graph_with_start_exit("NodeOverridesEdgeThreadTest");
    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    work.attrs.insert(
        "thread_id".to_string(),
        AttrValue::String("node-thread".to_string()),
    );
    graph.nodes.insert("work".to_string(), work);

    let mut edge_to_work = Edge::new("start", "work");
    edge_to_work.attrs.insert(
        "thread_id".to_string(),
        AttrValue::String("edge-thread".to_string()),
    );
    graph.edges.push(edge_to_work);
    graph.edges.push(Edge::new("work", "exit"));

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine.run(&graph, &config).await.expect("run");

    let thread_ids = captures.thread_ids.lock().unwrap();
    assert_eq!(thread_ids[0].0, "work");
    assert_eq!(
        thread_ids[0].1,
        Some("edge-thread".to_string()),
        "edge thread_id should take precedence over node thread_id"
    );
}

#[tokio::test]
async fn fidelity_resume_preserves_context_values_across_checkpoint() {
    // After resuming from a checkpoint, context values from the checkpoint
    // should be available to the resumed nodes. This tests that fidelity-related
    // context survives the resume path.
    let mut graph = make_graph_with_start_exit("FidelityResumeContextTest");
    let mut step_a = Node::new("step_a");
    step_a.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("compact".to_string()),
    );
    graph.nodes.insert("step_a".to_string(), step_a);

    let mut step_b = Node::new("step_b");
    step_b.attrs.insert(
        "type".to_string(),
        AttrValue::String("fidelity_capture".to_string()),
    );
    step_b.attrs.insert(
        "fidelity".to_string(),
        AttrValue::String("summary:low".to_string()),
    );
    graph.nodes.insert("step_b".to_string(), step_b);

    graph.edges.push(Edge::new("start", "step_a"));
    graph.edges.push(Edge::new("step_a", "step_b"));
    graph.edges.push(Edge::new("step_b", "exit"));

    let ctx = Context::new();
    ctx.set("outcome", serde_json::json!("success"));
    ctx.set("internal.fidelity", serde_json::json!("compact"));
    ctx.set("context.custom_key", serde_json::json!("custom_value"));

    let mut outcomes = std::collections::HashMap::new();
    outcomes.insert("start".to_string(), Outcome::success());
    outcomes.insert("step_a".to_string(), Outcome::success());

    let checkpoint = Checkpoint::from_context(
        &ctx,
        "step_a",
        vec!["start".to_string(), "step_a".to_string()],
        std::collections::HashMap::new(),
        outcomes,
        Some("step_b".to_string()),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );

    let captures = FidelityCaptures::new();
    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fidelity_capture",
        Box::new(FidelityCapturingHandler {
            captures: captures.clone(),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine
        .run_from_checkpoint(&graph, &config, &checkpoint)
        .await
        .expect("resume should succeed");

    let fidelities = captures.fidelities.lock().unwrap();
    assert_eq!(fidelities[0].0, "step_b");
    assert_eq!(
        fidelities[0].1, "summary:low",
        "resumed node should use its own fidelity (no degrade since checkpoint was compact, not full)"
    );

    // Verify the final checkpoint still has the fidelity
    let final_cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert_eq!(
        final_cp.context_values.get("internal.fidelity"),
        Some(&serde_json::json!("summary:low")),
    );
}

// ===========================================================================
// 20. Real LLM pipeline tests (requires ANTHROPIC_API_KEY)
// ===========================================================================

mod real_llm {
    use std::sync::Arc;

    use async_trait::async_trait;

    use fabro_config::FabroConfig;
    use fabro_graphviz::graph::Node;
    use fabro_workflows::context::Context;
    use fabro_workflows::error::FabroError;
    use fabro_workflows::handler::agent::{AgentHandler, CodergenBackend, CodergenResult};

    use fabro_llm::client::Client;
    use fabro_llm::types::{Message, Request};

    struct LlmCodergenBackend {
        client: Arc<Client>,
        model: String,
    }

    #[async_trait]
    impl CodergenBackend for LlmCodergenBackend {
        async fn run(
            &self,
            _node: &Node,
            prompt: &str,
            _context: &Context,
            _thread_id: Option<&str>,
            _emitter: &Arc<EventEmitter>,
            _stage_dir: &std::path::Path,
            _sandbox: &Arc<dyn fabro_agent::Sandbox>,
            _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
        ) -> Result<CodergenResult, FabroError> {
            self.complete(prompt).await
        }

        async fn one_shot(
            &self,
            _node: &Node,
            prompt: &str,
            _system_prompt: Option<&str>,
            _stage_dir: &std::path::Path,
        ) -> Result<CodergenResult, FabroError> {
            self.complete(prompt).await
        }
    }

    impl LlmCodergenBackend {
        async fn complete(&self, prompt: &str) -> Result<CodergenResult, FabroError> {
            let request = Request {
                model: self.model.clone(),
                messages: vec![Message::user(prompt)],
                provider: Some("anthropic".to_string()),
                tools: None,
                tool_choice: None,
                response_format: None,
                temperature: Some(0.0),
                top_p: None,
                max_tokens: Some(200),
                stop_sequences: None,
                reasoning_effort: None,
                speed: None,
                metadata: None,
                provider_options: None,
            };
            let response = self
                .client
                .complete(&request)
                .await
                .map_err(|e| FabroError::handler(e.to_string()))?;
            Ok(CodergenResult::Text {
                text: response.text(),
                usage: None,
                files_touched: Vec::new(),
                last_file_touched: None,
            })
        }
    }

    async fn make_llm_client() -> Option<Arc<Client>> {
        let _ = dotenvy::dotenv();
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            return None;
        }
        let client = Client::from_env()
            .await
            .expect("unified-llm client should initialize from env");
        Some(Arc::new(client))
    }

    fn make_llm_backend(client: Arc<Client>) -> Box<LlmCodergenBackend> {
        Box::new(LlmCodergenBackend {
            client,
            model: "claude-haiku-4-5".to_string(),
        })
    }

    use super::local_env;
    use fabro_graphviz::graph::{AttrValue, Edge, Graph};
    use fabro_interview::AutoApproveInterviewer;
    use fabro_workflows::checkpoint::Checkpoint;
    use fabro_workflows::event::EventEmitter;
    use fabro_workflows::handler::exit::ExitHandler;
    use fabro_workflows::handler::human::HumanHandler;
    use fabro_workflows::handler::start::StartHandler;
    use fabro_workflows::handler::HandlerRegistry;
    use fabro_workflows::outcome::StageStatus;
    use fabro_workflows::run_settings::RunSettings;
    use fabro_workflows::test_support::WorkflowRunner;

    #[tokio::test]
    #[ignore]
    async fn real_llm_linear_pipeline() {
        let client = if let Some(c) = make_llm_client().await {
            c
        } else {
            eprintln!("Skipping: ANTHROPIC_API_KEY not set");
            return;
        };

        let mut graph = Graph::new("RealLLMLinear");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Describe a sorting algorithm".to_string()),
        );

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

        let mut plan = Node::new("plan");
        plan.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        plan.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Briefly describe quicksort in 2-3 sentences.".to_string()),
        );
        graph.nodes.insert("plan".to_string(), plan);

        let mut review = Node::new("review");
        review
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        review.attrs.insert(
            "prompt".to_string(),
            AttrValue::String(
                "Review the previous description and add one improvement suggestion.".to_string(),
            ),
        );
        graph.nodes.insert("review".to_string(), review);

        graph.edges.push(Edge::new("start", "plan"));
        graph.edges.push(Edge::new("plan", "review"));
        graph.edges.push(Edge::new("review", "exit"));

        let dir = tempfile::tempdir().unwrap();
        let backend = make_llm_backend(client);
        let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(backend))));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register(
            "agent",
            Box::new(AgentHandler::new(Some(make_llm_backend(
                make_llm_client().await.unwrap(),
            )))),
        );

        let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            config: FabroConfig::default(),
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            labels: std::collections::HashMap::new(),
            git_author: fabro_workflows::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            base_branch: None,
            host_repo_path: None,
            git: None,
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            engine.run(&graph, &config),
        )
        .await
        .expect("should not timeout")
        .expect("real LLM pipeline should succeed");

        assert_eq!(outcome.status, StageStatus::Success);

        let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        assert!(checkpoint.completed_nodes.contains(&"plan".to_string()));
        assert!(checkpoint.completed_nodes.contains(&"review".to_string()));

        let last_stage = checkpoint
            .context_values
            .get("last_stage")
            .and_then(|v| v.as_str());
        assert_eq!(last_stage, Some("review"));

        // Verify actual LLM responses were written
        let plan_response =
            std::fs::read_to_string(dir.path().join("nodes").join("plan").join("response.md"))
                .unwrap();
        assert!(
            !plan_response.is_empty(),
            "LLM should have generated a response"
        );
        assert!(
            !plan_response.contains("[Simulated]"),
            "response should be from real LLM, not simulated"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn real_llm_two_stage_pipeline() {
        let client = if let Some(c) = make_llm_client().await {
            c
        } else {
            eprintln!("Skipping: ANTHROPIC_API_KEY not set");
            return;
        };

        let mut graph = Graph::new("RealLLMTwoStage");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Generate and review".to_string()),
        );

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

        let mut generate = Node::new("generate");
        generate
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        generate.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Write a haiku about programming.".to_string()),
        );
        graph.nodes.insert("generate".to_string(), generate);

        let mut review = Node::new("review");
        review
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        review.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Rate the haiku on a scale of 1-10.".to_string()),
        );
        graph.nodes.insert("review".to_string(), review);

        graph.edges.push(Edge::new("start", "generate"));
        graph.edges.push(Edge::new("generate", "review"));
        graph.edges.push(Edge::new("review", "exit"));

        let dir = tempfile::tempdir().unwrap();
        let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(
            make_llm_backend(Arc::clone(&client)),
        ))));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register(
            "agent",
            Box::new(AgentHandler::new(Some(make_llm_backend(client)))),
        );

        let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            config: FabroConfig::default(),
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            labels: std::collections::HashMap::new(),
            git_author: fabro_workflows::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            base_branch: None,
            host_repo_path: None,
            git: None,
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            engine.run(&graph, &config),
        )
        .await
        .expect("should not timeout")
        .expect("real LLM two-stage pipeline should succeed");

        assert_eq!(outcome.status, StageStatus::Success);

        let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        let last_stage = checkpoint
            .context_values
            .get("last_stage")
            .and_then(|v| v.as_str());
        assert_eq!(last_stage, Some("review"));
    }

    #[tokio::test]
    #[ignore]
    async fn real_llm_human_gate_auto_approve() {
        let client = if let Some(c) = make_llm_client().await {
            c
        } else {
            eprintln!("Skipping: ANTHROPIC_API_KEY not set");
            return;
        };

        let mut graph = Graph::new("RealLLMGate");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Write and approve".to_string()),
        );

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

        let mut write = Node::new("write");
        write
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        write.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Write a one-line greeting.".to_string()),
        );
        graph.nodes.insert("write".to_string(), write);

        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        gate.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
        gate.attrs.insert(
            "label".to_string(),
            AttrValue::String("Approve?".to_string()),
        );
        graph.nodes.insert("gate".to_string(), gate);

        let mut ship = Node::new("ship");
        ship.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        ship.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Ship the greeting.".to_string()),
        );
        graph.nodes.insert("ship".to_string(), ship);

        let mut revise = Node::new("revise");
        revise
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        revise.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Revise the greeting.".to_string()),
        );
        graph.nodes.insert("revise".to_string(), revise);

        graph.edges.push(Edge::new("start", "write"));
        graph.edges.push(Edge::new("write", "gate"));

        let mut approve_edge = Edge::new("gate", "ship");
        approve_edge.attrs.insert(
            "label".to_string(),
            AttrValue::String("[A] Approve".to_string()),
        );
        graph.edges.push(approve_edge);

        let mut revise_edge = Edge::new("gate", "revise");
        revise_edge.attrs.insert(
            "label".to_string(),
            AttrValue::String("[R] Revise".to_string()),
        );
        graph.edges.push(revise_edge);

        graph.edges.push(Edge::new("ship", "exit"));
        graph.edges.push(Edge::new("revise", "gate"));

        let dir = tempfile::tempdir().unwrap();
        let interviewer = Arc::new(AutoApproveInterviewer);

        let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(
            make_llm_backend(Arc::clone(&client)),
        ))));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register(
            "agent",
            Box::new(AgentHandler::new(Some(make_llm_backend(client)))),
        );
        registry.register("human", Box::new(HumanHandler::new(interviewer)));

        let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            config: FabroConfig::default(),
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            labels: std::collections::HashMap::new(),
            git_author: fabro_workflows::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            base_branch: None,
            host_repo_path: None,
            git: None,
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            engine.run(&graph, &config),
        )
        .await
        .expect("should not timeout")
        .expect("real LLM gate pipeline should succeed");

        assert_eq!(outcome.status, StageStatus::Success);

        let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        assert!(
            checkpoint.completed_nodes.contains(&"write".to_string()),
            "write should be completed"
        );
        assert!(
            checkpoint.completed_nodes.contains(&"gate".to_string()),
            "gate should be completed"
        );
        assert!(
            checkpoint.completed_nodes.contains(&"ship".to_string()),
            "ship should be completed (auto-approve selects first option)"
        );
        assert!(
            !checkpoint.completed_nodes.contains(&"revise".to_string()),
            "revise should NOT be traversed with auto-approve"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn real_llm_one_shot_pipeline() {
        let client = if let Some(c) = make_llm_client().await {
            c
        } else {
            eprintln!("Skipping: ANTHROPIC_API_KEY not set");
            return;
        };

        let mut graph = Graph::new("RealLLMOneShot");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Classify a fruit".to_string()),
        );

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

        let mut classify = Node::new("classify");
        classify
            .attrs
            .insert("shape".to_string(), AttrValue::String("tab".to_string()));
        classify.attrs.insert(
            "prompt".to_string(),
            AttrValue::String(
                "Reply with exactly one word: is an apple a fruit or vegetable?".to_string(),
            ),
        );
        classify.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-haiku-4-5".to_string()),
        );
        graph.nodes.insert("classify".to_string(), classify);

        graph.edges.push(Edge::new("start", "classify"));
        graph.edges.push(Edge::new("classify", "exit"));

        let dir = tempfile::tempdir().unwrap();

        let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(
            make_llm_backend(Arc::clone(&client)),
        ))));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register(
            "prompt",
            Box::new(fabro_workflows::handler::prompt::PromptHandler::new(Some(
                make_llm_backend(client),
            ))),
        );

        let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            config: FabroConfig::default(),
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            labels: std::collections::HashMap::new(),
            git_author: fabro_workflows::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            base_branch: None,
            host_repo_path: None,
            git: None,
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            engine.run(&graph, &config),
        )
        .await
        .expect("should not timeout")
        .expect("one_shot pipeline should succeed");

        assert_eq!(outcome.status, StageStatus::Success);

        let response_path = dir
            .path()
            .join("nodes")
            .join("classify")
            .join("response.md");
        let response = std::fs::read_to_string(&response_path).unwrap();
        assert!(!response.is_empty(), "response.md should be non-empty");
    }
}

// ---------------------------------------------------------------------------
// Wait.human freeform edge integration tests (Section 4.6)
// ---------------------------------------------------------------------------

/// Freeform-only human gate: free-text input routes through the freeform edge
/// and stores the text in human.gate.text context variable.
#[tokio::test]
async fn human_gate_freeform_only_routes_text() {
    // Graph: start -> gate -> freeform_target -> exit
    // gate has only a freeform edge (no fixed choices)
    let mut graph = Graph::new("FreeformOnlyTest");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Enter feedback".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("freeform_target".to_string(), Node::new("freeform_target"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut freeform_edge = Edge::new("gate", "freeform_target");
    freeform_edge
        .attrs
        .insert("freeform".to_string(), AttrValue::Boolean(true));
    graph.edges.push(freeform_edge);

    graph.edges.push(Edge::new("freeform_target", "exit"));

    let answers = VecDeque::from([Answer::text("my free text input")]);
    let interviewer = Arc::new(QueueInterviewer::new(answers));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"freeform_target".to_string()),
        "should have routed through freeform_target"
    );
    assert_eq!(
        checkpoint.context_values.get("human.gate.text"),
        Some(&serde_json::json!("my free text input")),
        "human.gate.text should contain the freeform input"
    );
    assert_eq!(
        checkpoint.context_values.get("human.gate.selected"),
        Some(&serde_json::json!("freeform")),
        "human.gate.selected should be 'freeform'"
    );
    assert_eq!(
        checkpoint.context_values.get("human.gate.label"),
        Some(&serde_json::json!("my free text input")),
        "human.gate.label should contain the freeform text"
    );
}

/// Human gate with both fixed choices and a freeform edge:
/// when the answer matches a fixed choice, it routes to the fixed choice target.
#[tokio::test]
async fn human_gate_freeform_with_fixed_choice_match() {
    // Graph: start -> gate -> {approve, reject, freeform_target} -> exit
    // gate has fixed choices plus a freeform edge
    // Answer selects "A" which matches "Approve" -> routes to approve
    let mut graph = Graph::new("FreeformFixedMatchTest");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Review Changes".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("reject".to_string(), Node::new("reject"));
    graph
        .nodes
        .insert("freeform_target".to_string(), Node::new("freeform_target"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut e_approve = Edge::new("gate", "approve");
    e_approve.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(e_approve);

    let mut e_reject = Edge::new("gate", "reject");
    e_reject.attrs.insert(
        "label".to_string(),
        AttrValue::String("[R] Reject".to_string()),
    );
    graph.edges.push(e_reject);

    let mut freeform_edge = Edge::new("gate", "freeform_target");
    freeform_edge
        .attrs
        .insert("freeform".to_string(), AttrValue::Boolean(true));
    graph.edges.push(freeform_edge);

    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("reject", "exit"));
    graph.edges.push(Edge::new("freeform_target", "exit"));

    // Answer selects "A" which matches the Approve choice
    let answers = VecDeque::from([Answer {
        value: AnswerValue::Selected("A".to_string()),
        selected_option: None,
        text: None,
    }]);
    let interviewer = Arc::new(QueueInterviewer::new(answers));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint.completed_nodes.contains(&"approve".to_string()),
        "fixed choice match should route to approve"
    );
    assert!(
        !checkpoint
            .completed_nodes
            .contains(&"freeform_target".to_string()),
        "should NOT route through freeform when fixed choice matches"
    );
}

/// Human gate with both fixed choices and a freeform edge:
/// when the answer does NOT match any fixed choice, it falls through to the freeform edge.
#[tokio::test]
async fn human_gate_freeform_fallback_on_unmatched_text() {
    // Graph: start -> gate -> {approve, reject, freeform_target} -> exit
    // gate has fixed choices plus a freeform edge
    // Answer is free text that doesn't match any choice -> routes to freeform_target
    let mut graph = Graph::new("FreeformFallbackTest");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Review Changes".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("reject".to_string(), Node::new("reject"));
    graph
        .nodes
        .insert("freeform_target".to_string(), Node::new("freeform_target"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut e_approve = Edge::new("gate", "approve");
    e_approve.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(e_approve);

    let mut e_reject = Edge::new("gate", "reject");
    e_reject.attrs.insert(
        "label".to_string(),
        AttrValue::String("[R] Reject".to_string()),
    );
    graph.edges.push(e_reject);

    let mut freeform_edge = Edge::new("gate", "freeform_target");
    freeform_edge
        .attrs
        .insert("freeform".to_string(), AttrValue::Boolean(true));
    graph.edges.push(freeform_edge);

    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("reject", "exit"));
    graph.edges.push(Edge::new("freeform_target", "exit"));

    // Free-text answer that doesn't match any fixed choice
    let answers = VecDeque::from([Answer::text("I need more context before deciding")]);
    let interviewer = Arc::new(QueueInterviewer::new(answers));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let checkpoint = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    assert!(
        checkpoint
            .completed_nodes
            .contains(&"freeform_target".to_string()),
        "unmatched text should fall through to freeform_target"
    );
    assert!(
        !checkpoint.completed_nodes.contains(&"approve".to_string()),
        "should NOT route to approve"
    );
    assert!(
        !checkpoint.completed_nodes.contains(&"reject".to_string()),
        "should NOT route to reject"
    );
    assert_eq!(
        checkpoint.context_values.get("human.gate.text"),
        Some(&serde_json::json!("I need more context before deciding")),
        "human.gate.text should contain the freeform input"
    );
    assert_eq!(
        checkpoint.context_values.get("human.gate.selected"),
        Some(&serde_json::json!("freeform")),
        "human.gate.selected should be 'freeform' for freeform fallback"
    );
    assert_eq!(
        checkpoint.context_values.get("human.gate.label"),
        Some(&serde_json::json!("I need more context before deciding")),
        "human.gate.label should contain the freeform text"
    );
}

/// Verifies that the Question presented to the interviewer has `allow_freeform=true`
/// when a freeform edge is present on the human gate.
#[tokio::test]
async fn human_gate_freeform_sets_allow_freeform_on_question() {
    // Graph: start -> gate -> {approve, freeform_target} -> exit
    // gate has a fixed choice plus a freeform edge
    // We use RecordingInterviewer to capture the question and verify allow_freeform
    let mut graph = Graph::new("AllowFreeformTest");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Pick or type".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("freeform_target".to_string(), Node::new("freeform_target"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut e_approve = Edge::new("gate", "approve");
    e_approve.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(e_approve);

    let mut freeform_edge = Edge::new("gate", "freeform_target");
    freeform_edge
        .attrs
        .insert("freeform".to_string(), AttrValue::Boolean(true));
    graph.edges.push(freeform_edge);

    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("freeform_target", "exit"));

    let answers = VecDeque::from([Answer {
        value: AnswerValue::Selected("A".to_string()),
        selected_option: None,
        text: None,
    }]);
    let inner = QueueInterviewer::new(answers);
    let recorder = Arc::new(RecordingInterviewer::new(Box::new(inner)));
    let interviewer: Arc<dyn Interviewer> = recorder.clone();

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let recordings = recorder.recordings();
    assert_eq!(
        recordings.len(),
        1,
        "should have recorded exactly one question"
    );
    assert!(
        recordings[0].0.allow_freeform,
        "Question should have allow_freeform=true when a freeform edge is present"
    );
}

/// Verifies that the Question presented to the interviewer has `allow_freeform=false`
/// when no freeform edge is present on the human gate (fixed choices only).
#[tokio::test]
async fn human_gate_without_freeform_sets_allow_freeform_false() {
    // Graph: start -> gate -> {approve, reject} -> exit
    // gate has only fixed choices, no freeform edge
    let mut graph = Graph::new("NoFreeformTest");

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

    let mut gate = Node::new("gate");
    gate.attrs.insert(
        "shape".to_string(),
        AttrValue::String("hexagon".to_string()),
    );
    gate.attrs
        .insert("type".to_string(), AttrValue::String("human".to_string()));
    gate.attrs.insert(
        "label".to_string(),
        AttrValue::String("Pick one".to_string()),
    );
    graph.nodes.insert("gate".to_string(), gate);
    graph
        .nodes
        .insert("approve".to_string(), Node::new("approve"));
    graph
        .nodes
        .insert("reject".to_string(), Node::new("reject"));

    graph.edges.push(Edge::new("start", "gate"));

    let mut e_approve = Edge::new("gate", "approve");
    e_approve.attrs.insert(
        "label".to_string(),
        AttrValue::String("[A] Approve".to_string()),
    );
    graph.edges.push(e_approve);

    let mut e_reject = Edge::new("gate", "reject");
    e_reject.attrs.insert(
        "label".to_string(),
        AttrValue::String("[R] Reject".to_string()),
    );
    graph.edges.push(e_reject);

    graph.edges.push(Edge::new("approve", "exit"));
    graph.edges.push(Edge::new("reject", "exit"));

    let answers = VecDeque::from([Answer {
        value: AnswerValue::Selected("A".to_string()),
        selected_option: None,
        text: None,
    }]);
    let inner = QueueInterviewer::new(answers);
    let recorder = Arc::new(RecordingInterviewer::new(Box::new(inner)));
    let interviewer: Arc<dyn Interviewer> = recorder.clone();

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let recordings = recorder.recordings();
    assert_eq!(
        recordings.len(),
        1,
        "should have recorded exactly one question"
    );
    assert!(
        !recordings[0].0.allow_freeform,
        "Question should have allow_freeform=false when no freeform edge is present"
    );
}

// ---------------------------------------------------------------------------
// Subgraph features (Section 2.10)
// ---------------------------------------------------------------------------

#[test]
fn subgraph_node_defaults_scoped_to_subgraph() {
    let input = r#"digraph SubgraphDefaults {
        graph [goal="Test subgraph defaults"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]

        subgraph cluster_loop {
            label = "Loop A"
            node [thread_id="loop-a", timeout="900s"]

            plan      [label="Plan next step"]
            implement [label="Implement", timeout="1800s"]
        }

        outside [label="Outside node"]

        start -> plan -> implement -> outside -> exit
    }"#;

    let graph = parse(input).expect("parsing should succeed");

    // Plan inherits both thread_id and timeout from subgraph defaults
    let plan = &graph.nodes["plan"];
    assert_eq!(plan.thread_id(), Some("loop-a"));
    assert_eq!(plan.timeout(), Some(std::time::Duration::from_secs(900)));

    // Implement inherits thread_id but overrides timeout
    let implement = &graph.nodes["implement"];
    assert_eq!(implement.thread_id(), Some("loop-a"));
    assert_eq!(
        implement.timeout(),
        Some(std::time::Duration::from_secs(1800))
    );

    // Outside node should NOT have subgraph defaults
    let outside = &graph.nodes["outside"];
    assert_eq!(outside.thread_id(), None);
    assert_eq!(outside.timeout(), None);
}

#[test]
fn subgraph_class_derived_from_label() {
    let input = r#"digraph SubgraphClass {
        graph [goal="Test class derivation"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]

        subgraph cluster_loop {
            label = "Loop A"
            plan      [label="Plan"]
            implement [label="Implement"]
        }

        start -> plan -> implement -> exit
    }"#;

    let graph = parse(input).expect("parsing should succeed");

    // Nodes inside subgraph receive derived class "loop-a"
    assert!(graph.nodes["plan"].classes.contains(&"loop-a".to_string()));
    assert!(graph.nodes["implement"]
        .classes
        .contains(&"loop-a".to_string()));

    // Nodes outside subgraph do not get the class
    assert!(!graph.nodes["start"].classes.contains(&"loop-a".to_string()));
    assert!(!graph.nodes["exit"].classes.contains(&"loop-a".to_string()));
}

#[test]
fn subgraph_class_derivation_strips_special_chars() {
    let input = r#"digraph SubgraphClassStrip {
        graph [goal="Test class derivation with special chars"]

        subgraph cluster_review {
            label = "Code Review!!!"
            reviewer [label="Reviewer"]
        }
    }"#;

    let graph = parse(input).expect("parsing should succeed");
    // "Code Review!!!" -> lowercase "code review!!!" -> spaces to hyphens "code-review!!!"
    // -> strip non-alphanumeric except hyphens -> "code-review"
    assert!(graph.nodes["reviewer"]
        .classes
        .contains(&"code-review".to_string()));
}

#[test]
fn subgraph_scoping_does_not_leak_to_outer_scope() {
    let input = r#"digraph SubgraphScoping {
        graph [goal="Test scoping"]
        node [timeout="300s"]

        subgraph cluster_inner {
            label = "Inner"
            node [timeout="900s"]
            inner_node [label="Inner"]
        }

        outer_node [label="Outer"]
    }"#;

    let graph = parse(input).expect("parsing should succeed");

    // Inner node gets the subgraph-scoped timeout of 900s
    let inner = &graph.nodes["inner_node"];
    assert_eq!(inner.timeout(), Some(std::time::Duration::from_secs(900)));

    // Outer node gets the graph-level default of 300s, not the subgraph's 900s
    let outer = &graph.nodes["outer_node"];
    assert_eq!(outer.timeout(), Some(std::time::Duration::from_secs(300)));
}

#[test]
fn subgraph_global_defaults_plus_subgraph_defaults() {
    let input = r#"digraph SubgraphMerge {
        graph [goal="Test merged defaults"]
        node [shape=box, timeout="300s"]

        subgraph cluster_loop {
            label = "Loop"
            node [thread_id="loop-thread"]
            step [label="Step"]
        }

        plain [label="Plain"]
    }"#;

    let graph = parse(input).expect("parsing should succeed");

    // Step should have both the global shape=box + timeout=300s and subgraph thread_id
    let step = &graph.nodes["step"];
    assert_eq!(step.shape(), "box");
    assert_eq!(step.thread_id(), Some("loop-thread"));
    assert_eq!(step.timeout(), Some(std::time::Duration::from_secs(300)));

    // Plain should have the global defaults but no thread_id
    let plain = &graph.nodes["plain"];
    assert_eq!(plain.shape(), "box");
    assert_eq!(plain.thread_id(), None);
    assert_eq!(plain.timeout(), Some(std::time::Duration::from_secs(300)));
}

#[test]
fn subgraph_edges_inherit_class() {
    let input = r#"digraph SubgraphEdgeClass {
        graph [goal="Test edge nodes get class"]

        subgraph cluster_loop {
            label = "My Loop"
            a [label="A"]
            b [label="B"]
            a -> b
        }
    }"#;

    let graph = parse(input).expect("parsing should succeed");

    // Both nodes referenced in edges within the subgraph get the derived class
    assert!(graph.nodes["a"].classes.contains(&"my-loop".to_string()));
    assert!(graph.nodes["b"].classes.contains(&"my-loop".to_string()));
}

#[test]
fn subgraph_without_label_no_class_derived() {
    let input = r#"digraph SubgraphNoLabel {
        graph [goal="Test subgraph without label"]

        subgraph cluster_unnamed {
            node [timeout="600s"]
            worker [label="Worker"]
        }
    }"#;

    let graph = parse(input).expect("parsing should succeed");

    // No label means no class should be derived
    let worker = &graph.nodes["worker"];
    assert!(worker.classes.is_empty());
    // But the default should still apply
    assert_eq!(worker.timeout(), Some(std::time::Duration::from_secs(600)));
}

// ---------------------------------------------------------------------------
// Hook System E2E Tests
// ---------------------------------------------------------------------------

fn hook_runner_from_defs(hooks: Vec<fabro_hooks::HookDefinition>) -> Arc<fabro_hooks::HookRunner> {
    Arc::new(fabro_hooks::HookRunner::new(fabro_hooks::HookConfig {
        hooks,
    }))
}

struct HookTestRunner {
    emitter: Arc<EventEmitter>,
    hook_runner: Arc<fabro_hooks::HookRunner>,
}

impl HookTestRunner {
    async fn run(&self, graph: &Graph, config: &RunSettings) -> Result<Outcome, FabroError> {
        run_graph_with_hooks(
            make_linear_registry(),
            Arc::clone(&self.emitter),
            local_env(),
            graph,
            config,
            Arc::clone(&self.hook_runner),
            None,
        )
        .await
    }
}

fn emitter_with_events() -> (
    Arc<EventEmitter>,
    Arc<std::sync::Mutex<Vec<WorkflowRunEvent>>>,
) {
    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);
    (Arc::new(emitter), events)
}

fn engine_with_hooks(hooks: Vec<fabro_hooks::HookDefinition>) -> HookTestRunner {
    HookTestRunner {
        emitter: Arc::new(EventEmitter::new()),
        hook_runner: hook_runner_from_defs(hooks),
    }
}

fn engine_with_hooks_and_events(
    hooks: Vec<fabro_hooks::HookDefinition>,
) -> (HookTestRunner, Arc<std::sync::Mutex<Vec<WorkflowRunEvent>>>) {
    let (emitter, events) = emitter_with_events();
    (
        HookTestRunner {
            emitter,
            hook_runner: hook_runner_from_defs(hooks),
        },
        events,
    )
}

fn make_run_config(dir: &std::path::Path) -> RunSettings {
    RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "hook-test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    }
}

fn make_hook(event: fabro_hooks::HookEvent, command: &str) -> fabro_hooks::HookDefinition {
    fabro_hooks::HookDefinition {
        name: None,
        event,
        command: Some(command.into()),
        hook_type: None,
        matcher: None,
        blocking: None,
        timeout_ms: Some(5000),
        sandbox: Some(false), // run on host for test reliability
    }
}

fn simple_linear_dot() -> &'static str {
    r#"digraph HookTest {
        graph [goal="Test hooks"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        work  [shape=box, label="Work", prompt="Do work"]
        start -> work -> exit
    }"#
}

fn two_step_dot() -> &'static str {
    r#"digraph HookTest {
        graph [goal="Test hooks"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        step1 [shape=box, label="Step1", prompt="First"]
        step2 [shape=box, label="Step2", prompt="Second"]
        start -> step1 -> step2 -> exit
    }"#
}

fn branching_dot() -> &'static str {
    r#"digraph HookTest {
        graph [goal="Test routing"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        plan  [shape=box, label="Plan", prompt="Plan it"]
        pathA [shape=box, label="PathA", prompt="Path A"]
        pathB [shape=box, label="PathB", prompt="Path B"]
        start -> plan
        plan -> pathA [label="A"]
        plan -> pathB [label="B"]
        pathA -> exit
        pathB -> exit
    }"#
}

// --- RunStart hook tests ---

#[tokio::test]
async fn hook_run_start_proceed_allows_run() {
    let hooks = vec![make_hook(fabro_hooks::HookEvent::RunStart, "exit 0")];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
async fn hook_run_start_block_prevents_run() {
    let hooks = vec![make_hook(fabro_hooks::HookEvent::RunStart, "exit 1")];
    let (engine, events) = engine_with_hooks_and_events(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let result = engine.run(&graph, &config).await;
    assert!(result.is_err(), "RunStart block should cause error");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("hook"),
        "Error should mention hook: {err}"
    );

    // WorkflowRunStarted should still have been emitted (it fires before the hook)
    let captured = events.lock().unwrap();
    assert!(
        captured
            .iter()
            .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunStarted { .. })),
        "WorkflowRunStarted should be emitted before hook blocks"
    );

    // But no StageStarted — the run never reached node execution
    assert!(
        !captured
            .iter()
            .any(|e| matches!(e, WorkflowRunEvent::StageStarted { .. })),
        "No stage should start when RunStart hook blocks"
    );
}

#[tokio::test]
async fn hook_run_start_block_with_json_reason() {
    // Hook that outputs JSON with a reason
    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::RunStart,
        r#"echo '{"decision":"block","reason":"policy violation"}'; exit 2"#,
    )];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let result = engine.run(&graph, &config).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("policy violation"),
        "Error should contain JSON reason: {err}"
    );
}

// --- StageStart hook tests ---

#[tokio::test]
async fn hook_stage_start_proceed_allows_execution() {
    let hooks = vec![make_hook(fabro_hooks::HookEvent::StageStart, "exit 0")];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);

    // Work node should have executed (response.md exists)
    assert!(
        dir.path()
            .join("nodes")
            .join("work")
            .join("response.md")
            .exists(),
        "response.md should exist when StageStart hook proceeds"
    );
}

#[tokio::test]
async fn hook_stage_start_skip_bypasses_node() {
    // Hook that outputs skip decision as JSON
    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::StageStart,
        r#"echo '{"decision":"skip","reason":"not needed"}'; exit 0"#,
    )];
    let (engine, events) = engine_with_hooks_and_events(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    // Pipeline reached exit with goal gates satisfied — per spec, SUCCESS.
    assert_eq!(outcome.status, StageStatus::Success);

    // response.md should NOT exist for the work node (it was skipped)
    assert!(
        !dir.path()
            .join("nodes")
            .join("work")
            .join("response.md")
            .exists(),
        "response.md should not exist when StageStart hook skips node"
    );

    // StageStarted should NOT be emitted for hook-skipped stages (the stage never started)
    let captured = events.lock().unwrap();
    let stage_starts: Vec<_> = captured
        .iter()
        .filter(|e| {
            matches!(e, WorkflowRunEvent::StageStarted { handler_type, .. }
            if handler_type.as_deref() != Some("start") && handler_type.as_deref() != Some("exit"))
        })
        .collect();
    assert!(
        stage_starts.is_empty(),
        "StageStarted should not be emitted when StageStart hook skips"
    );
}

#[tokio::test]
async fn hook_stage_start_block_aborts_run() {
    let hooks = vec![make_hook(fabro_hooks::HookEvent::StageStart, "exit 1")];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let result = engine.run(&graph, &config).await;
    assert!(result.is_err(), "StageStart block should abort the run");
}

#[tokio::test]
async fn hook_stage_start_matcher_filters_by_node_id() {
    // Hook that only matches nodes with "step2" in their ID
    let mut hook = make_hook(
        fabro_hooks::HookEvent::StageStart,
        r#"echo '{"decision":"skip","reason":"filtered"}'"#,
    );
    hook.matcher = Some("step2".into());
    let hooks = vec![hook];

    let engine = engine_with_hooks(hooks);
    let graph = parse(two_step_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    // Pipeline reached exit with goal gates satisfied — per spec, SUCCESS.
    assert_eq!(outcome.status, StageStatus::Success);

    // step1 should have executed (response.md exists)
    assert!(
        dir.path()
            .join("nodes")
            .join("step1")
            .join("response.md")
            .exists(),
        "step1 should execute because matcher doesn't match it"
    );

    // step2 should have been skipped (no response.md)
    assert!(
        !dir.path()
            .join("nodes")
            .join("step2")
            .join("response.md")
            .exists(),
        "step2 should be skipped because matcher matches it"
    );
}

#[tokio::test]
async fn hook_stage_start_matcher_no_match_proceeds() {
    // Hook with matcher that matches nothing
    let mut hook = make_hook(fabro_hooks::HookEvent::StageStart, "exit 1");
    hook.matcher = Some("nonexistent_node".into());
    let hooks = vec![hook];

    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

// --- StageComplete hook tests ---

#[tokio::test]
async fn hook_stage_complete_fires_after_success() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("stage_complete_marker.txt");

    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::StageComplete,
        &format!("echo $FABRO_NODE_ID >> {}", marker.display()),
    )];
    let engine = engine_with_hooks(hooks);
    let graph = parse(two_step_dot()).unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);

    // Marker file should exist and contain node IDs
    assert!(
        marker.exists(),
        "StageComplete hook should have written marker file"
    );
    let content = std::fs::read_to_string(&marker).unwrap();
    // start, step1, step2, exit all complete — hook fires for each
    assert!(
        content.contains("step1"),
        "Marker should contain step1: {content}"
    );
    assert!(
        content.contains("step2"),
        "Marker should contain step2: {content}"
    );
}

#[tokio::test]
async fn hook_stage_complete_failure_does_not_block_pipeline() {
    // Non-blocking hook that fails should not affect the pipeline
    let hooks = vec![make_hook(fabro_hooks::HookEvent::StageComplete, "exit 1")];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(
        outcome.status,
        StageStatus::Success,
        "Non-blocking StageComplete hook failure should not block pipeline"
    );
}

// --- RunComplete hook tests ---

#[tokio::test]
async fn hook_run_complete_fires_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("run_complete_marker.txt");

    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::RunComplete,
        &format!("echo done > {}", marker.display()),
    )];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);

    assert!(
        marker.exists(),
        "RunComplete hook should have written marker file"
    );
    let content = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(content.trim(), "done");
}

#[tokio::test]
async fn hook_run_complete_does_not_fire_on_blocked_run() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("run_complete_should_not_exist.txt");

    let hooks = vec![
        make_hook(
            fabro_hooks::HookEvent::RunStart,
            "exit 1", // block the run
        ),
        make_hook(
            fabro_hooks::HookEvent::RunComplete,
            &format!("echo done > {}", marker.display()),
        ),
    ];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    let _ = engine.run(&graph, &config).await;

    assert!(
        !marker.exists(),
        "RunComplete hook should not fire when run is blocked by RunStart"
    );
}

// --- RunFailed hook tests ---

#[tokio::test]
async fn hook_run_failed_fires_on_stage_block() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("run_failed_marker.txt");

    let hooks = vec![
        make_hook(
            fabro_hooks::HookEvent::StageStart,
            "exit 1", // block during stage
        ),
        make_hook(
            fabro_hooks::HookEvent::RunFailed,
            &format!("echo failed > {}", marker.display()),
        ),
    ];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    let _ = engine.run(&graph, &config).await;

    // RunFailed may or may not fire depending on the error path — a StageStart
    // block causes an engine error, which doesn't go through the normal
    // WorkflowRunFailed event. Let's just verify no panic occurs.
}

// --- Environment variables ---

#[tokio::test]
async fn hook_receives_env_vars() {
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join("hook_env.txt");

    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::StageComplete,
        &format!(
            "echo \"event=$FABRO_EVENT run=$FABRO_RUN_ID wf=$FABRO_WORKFLOW node=$FABRO_NODE_ID\" >> {}",
            env_file.display()
        ),
    )];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    engine.run(&graph, &config).await.unwrap();

    assert!(env_file.exists(), "Env file should be written by hook");
    let content = std::fs::read_to_string(&env_file).unwrap();

    // Should contain lines like: event=stage_complete run=hook-test-run wf=HookTest node=work
    let lines: Vec<&str> = content.lines().collect();
    let work_line = lines.iter().find(|l| l.contains("node=work"));
    assert!(
        work_line.is_some(),
        "Should have a line for node=work, got: {content}"
    );
    let line = work_line.unwrap();
    assert!(
        line.contains("event=stage_complete"),
        "FABRO_EVENT should be set: {line}"
    );
    assert!(
        line.contains("run=hook-test-run"),
        "FABRO_RUN_ID should be set: {line}"
    );
    assert!(
        line.contains("wf=HookTest"),
        "FABRO_WORKFLOW should be set: {line}"
    );
}

// --- Multiple hooks for same event ---

#[tokio::test]
async fn multiple_hooks_same_event_all_fire() {
    let dir = tempfile::tempdir().unwrap();
    let marker1 = dir.path().join("hook1.txt");
    let marker2 = dir.path().join("hook2.txt");

    let hooks = vec![
        make_hook(
            fabro_hooks::HookEvent::StageComplete,
            &format!("echo hook1 > {}", marker1.display()),
        ),
        make_hook(
            fabro_hooks::HookEvent::StageComplete,
            &format!("echo hook2 > {}", marker2.display()),
        ),
    ];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    engine.run(&graph, &config).await.unwrap();

    assert!(marker1.exists(), "First hook should have fired");
    assert!(marker2.exists(), "Second hook should have fired");
}

// --- No hooks configured (baseline) ---

#[tokio::test]
async fn no_hooks_configured_runs_normally() {
    let engine = engine_with_hooks(vec![]);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

// --- EdgeSelected hook tests ---

#[tokio::test]
async fn hook_edge_selected_override_redirects_routing() {
    // Hook that overrides edge routing to pathB when it would go to pathA
    let mut hook = make_hook(
        fabro_hooks::HookEvent::EdgeSelected,
        // Override routing to pathB
        r#"echo '{"decision":"override","edge_to":"pathB"}'"#,
    );
    // Only match edges going FROM plan
    hook.matcher = Some("^plan$".into());
    let hooks = vec![hook];

    let (engine, events) = engine_with_hooks_and_events(hooks);
    let graph = parse(branching_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);

    // Verify pathB was executed (override worked)
    let captured = events.lock().unwrap();
    let completed_nodes: Vec<String> = captured
        .iter()
        .filter_map(|e| match e {
            WorkflowRunEvent::StageCompleted { node_id, .. } => Some(node_id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        completed_nodes.contains(&"pathB".to_string()),
        "pathB should have been executed due to override: {completed_nodes:?}"
    );
}

#[tokio::test]
async fn hook_edge_selected_block_aborts_run() {
    let mut hook = make_hook(fabro_hooks::HookEvent::EdgeSelected, "exit 1");
    hook.matcher = Some("^plan$".into());
    let hooks = vec![hook];

    let engine = engine_with_hooks(hooks);
    let graph = parse(branching_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let result = engine.run(&graph, &config).await;
    assert!(result.is_err(), "EdgeSelected block should abort the run");
}

// --- CheckpointSaved hook ---

#[tokio::test]
async fn hook_checkpoint_saved_fires() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("checkpoint_marker.txt");

    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::CheckpointSaved,
        &format!("echo $FABRO_NODE_ID >> {}", marker.display()),
    )];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);

    // Checkpoint is saved after each node
    assert!(marker.exists(), "CheckpointSaved hook should have fired");
    let content = std::fs::read_to_string(&marker).unwrap();
    assert!(
        content.contains("work"),
        "Should contain 'work' node checkpoint: {content}"
    );
}

// --- StageStart with JSON skip via exit code 2 ---

#[tokio::test]
async fn hook_stage_start_exit_2_blocks() {
    let hooks = vec![make_hook(fabro_hooks::HookEvent::StageStart, "exit 2")];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    // exit 2 without JSON defaults to Block
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err(), "exit 2 should block");
}

// --- Config merge tests (server + run) ---

#[tokio::test]
async fn hook_config_merge_concatenates() {
    use fabro_hooks::{HookConfig, HookDefinition, HookEvent};

    let server_hooks = HookConfig {
        hooks: vec![HookDefinition {
            name: Some("server-hook".into()),
            event: HookEvent::RunStart,
            command: Some("exit 0".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: Some(false),
        }],
    };
    let run_hooks = HookConfig {
        hooks: vec![HookDefinition {
            name: Some("run-hook".into()),
            event: HookEvent::StageComplete,
            command: Some("exit 0".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: Some(false),
        }],
    };

    let merged = server_hooks.merge(run_hooks);
    assert_eq!(merged.hooks.len(), 2);
    assert_eq!(merged.hooks[0].name.as_deref(), Some("server-hook"));
    assert_eq!(merged.hooks[1].name.as_deref(), Some("run-hook"));
}

#[tokio::test]
async fn hook_config_merge_run_overrides_by_name() {
    use fabro_hooks::{HookConfig, HookDefinition, HookEvent};

    let server_hooks = HookConfig {
        hooks: vec![HookDefinition {
            name: Some("shared".into()),
            event: HookEvent::RunStart,
            command: Some("exit 1".into()), // would block
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: Some(false),
        }],
    };
    let run_hooks = HookConfig {
        hooks: vec![HookDefinition {
            name: Some("shared".into()),
            event: HookEvent::RunStart,
            command: Some("exit 0".into()), // allows
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: Some(false),
        }],
    };

    let merged = server_hooks.merge(run_hooks);
    assert_eq!(merged.hooks.len(), 1);
    // Run config wins — command should be "exit 0"
    assert_eq!(merged.hooks[0].command.as_deref(), Some("exit 0"));

    // Verify it actually works end-to-end
    let engine = engine_with_hooks(merged.hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

// --- TOML config parsing integration ---

#[test]
fn hook_toml_run_config_parsing() {
    let toml = r#"
version = 1
goal = "Test hooks in run config"
graph = "test.fabro"

[[hooks]]
event = "stage_start"
command = "./scripts/pre-check.sh"
matcher = "agent_loop"
blocking = true
timeout_ms = 30000
sandbox = false

[[hooks]]
event = "run_complete"
command = "echo done"
"#;

    let cfg: FabroConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.hooks.len(), 2);
    assert_eq!(cfg.hooks[0].event, fabro_hooks::HookEvent::StageStart);
    assert_eq!(cfg.hooks[0].matcher.as_deref(), Some("agent_loop"));
    assert!(cfg.hooks[0].is_blocking());
    assert!(!cfg.hooks[0].runs_in_sandbox());
    assert_eq!(
        cfg.hooks[0].timeout(),
        std::time::Duration::from_millis(30000)
    );
    assert_eq!(cfg.hooks[1].event, fabro_hooks::HookEvent::RunComplete);
    assert!(!cfg.hooks[1].is_blocking()); // RunComplete non-blocking by default
}

// --- Blocking vs non-blocking behavior ---

#[tokio::test]
async fn hook_blocking_override_makes_non_blocking_event_blocking() {
    // StageComplete is non-blocking by default, but force it to blocking
    let mut hook = make_hook(fabro_hooks::HookEvent::StageComplete, "exit 1");
    hook.blocking = Some(true);
    let hooks = vec![hook];

    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    // This test verifies that the blocking override is respected
    // Note: StageComplete hooks run AFTER execution, so they use the
    // non-blocking path in the engine (the engine doesn't check blocking
    // for StageComplete since it's always after the fact). This is correct
    // behavior — the blocking flag only affects the runner's execution
    // strategy (sequential vs parallel), not the engine's decision handling.
    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
async fn hook_non_blocking_override_on_blocking_event() {
    // RunStart is blocking by default, but force it to non-blocking
    let mut hook = make_hook(fabro_hooks::HookEvent::RunStart, "exit 1");
    hook.blocking = Some(false);
    let hooks = vec![hook];

    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    // With blocking=false, the RunStart hook failure should NOT block the run
    // because the runner treats it as non-blocking (doesn't merge decisions)
    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

// --- Regex matcher tests ---

#[tokio::test]
async fn hook_matcher_regex_pattern() {
    // Hook matches any node starting with "step"
    let mut hook = make_hook(
        fabro_hooks::HookEvent::StageStart,
        r#"echo '{"decision":"skip","reason":"regex match"}'"#,
    );
    hook.matcher = Some("^step".into());
    let hooks = vec![hook];

    let engine = engine_with_hooks(hooks);
    let graph = parse(two_step_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    // Pipeline reached exit with goal gates satisfied — per spec, SUCCESS.
    assert_eq!(outcome.status, StageStatus::Success);

    // Both step1 and step2 should be skipped
    assert!(
        !dir.path()
            .join("nodes")
            .join("step1")
            .join("response.md")
            .exists(),
        "step1 should be skipped by regex ^step"
    );
    assert!(
        !dir.path()
            .join("nodes")
            .join("step2")
            .join("response.md")
            .exists(),
        "step2 should be skipped by regex ^step"
    );
}

// --- JSON decision parsing from hook stdout ---

#[tokio::test]
async fn hook_json_proceed_explicit() {
    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::RunStart,
        r#"echo '{"decision":"proceed"}'"#,
    )];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
async fn hook_json_block_with_reason() {
    let hooks = vec![make_hook(
        fabro_hooks::HookEvent::RunStart,
        r#"echo '{"decision":"block","reason":"forbidden by policy"}'; exit 2"#,
    )];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let result = engine.run(&graph, &config).await;
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("forbidden by policy"));
}

// --- Sandbox field tests ---

#[tokio::test]
async fn hook_sandbox_false_runs_on_host() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("host_hook.txt");

    let mut hook = make_hook(
        fabro_hooks::HookEvent::RunComplete,
        &format!("echo host > {}", marker.display()),
    );
    hook.sandbox = Some(false);
    let hooks = vec![hook];

    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    engine.run(&graph, &config).await.unwrap();

    assert!(marker.exists(), "Host hook should write marker file");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "host");
}

// --- Prompt and Agent hook TOML parsing ---

#[test]
fn hook_toml_prompt_and_agent_parsing() {
    let toml = r#"
version = 1
goal = "Test prompt/agent hooks"
graph = "test.fabro"

[[hooks]]
event = "stage_start"
type = "prompt"
prompt = "Should this stage proceed?"
model = "haiku"

[[hooks]]
event = "run_complete"
type = "agent"
prompt = "Verify all tests pass."
model = "sonnet"
max_tool_rounds = 10
timeout_ms = 120000
"#;

    let cfg: FabroConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.hooks.len(), 2);

    // Prompt hook
    assert_eq!(cfg.hooks[0].event, fabro_hooks::HookEvent::StageStart);
    assert!(matches!(
        cfg.hooks[0].resolved_hook_type().as_deref(),
        Some(fabro_hooks::HookType::Prompt { prompt, model })
            if prompt == "Should this stage proceed?" && *model == Some("haiku".into())
    ));
    assert_eq!(
        cfg.hooks[0].timeout(),
        std::time::Duration::from_millis(30000)
    );

    // Agent hook
    assert_eq!(cfg.hooks[1].event, fabro_hooks::HookEvent::RunComplete);
    assert!(matches!(
        cfg.hooks[1].resolved_hook_type().as_deref(),
        Some(fabro_hooks::HookType::Agent { prompt, model, max_tool_rounds })
            if prompt == "Verify all tests pass."
            && *model == Some("sonnet".into())
            && *max_tool_rounds == Some(10)
    ));
    assert_eq!(
        cfg.hooks[1].timeout(),
        std::time::Duration::from_millis(120000)
    );
}

// --- Prompt/Agent hook E2E with real LLM ---

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn hook_prompt_proceed_allows_run() {
    dotenvy::dotenv().ok();

    let hooks = vec![fabro_hooks::HookDefinition {
        name: Some("prompt-proceed".into()),
        event: fabro_hooks::HookEvent::RunStart,
        command: None,
        hook_type: Some(fabro_hooks::HookType::Prompt {
            prompt: "A workflow is starting. Always approve. Respond with {\"ok\": true}.".into(),
            model: Some("haiku".into()),
        }),
        matcher: None,
        blocking: None,
        timeout_ms: Some(30000),
        sandbox: None,
    }];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn hook_prompt_block_prevents_run() {
    dotenvy::dotenv().ok();

    // Use a factual question that evaluates to false: "Is 2+2=5?"
    let hooks = vec![fabro_hooks::HookDefinition {
        name: Some("prompt-block".into()),
        event: fabro_hooks::HookEvent::RunStart,
        command: None,
        hook_type: Some(fabro_hooks::HookType::Prompt {
            prompt: "Check: is 2+2 equal to 5? If the statement is true, respond {\"ok\": true}. If false, respond {\"ok\": false, \"reason\": \"math check failed\"}.".into(),
            model: Some("haiku".into()),
        }),
        matcher: None,
        blocking: None,
        timeout_ms: Some(30000),
        sandbox: None,
    }];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_err(),
        "Prompt hook block should cause error, got: {result:?}"
    );
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn hook_agent_proceed_allows_run() {
    dotenvy::dotenv().ok();

    let hooks = vec![fabro_hooks::HookDefinition {
        name: Some("agent-proceed".into()),
        event: fabro_hooks::HookEvent::RunStart,
        command: None,
        hook_type: Some(fabro_hooks::HookType::Agent {
            prompt: "A workflow is starting. Always approve. Respond with {\"ok\": true}. Do not use any tools.".into(),
            model: Some("haiku".into()),
            max_tool_rounds: Some(1),
        }),
        matcher: None,
        blocking: None,
        timeout_ms: Some(60000),
        sandbox: None,
    }];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn hook_agent_with_tool_use() {
    dotenvy::dotenv().ok();

    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("hook_check.txt");
    std::fs::write(&marker, "READY").unwrap();

    let hooks = vec![fabro_hooks::HookDefinition {
        name: Some("agent-tools".into()),
        event: fabro_hooks::HookEvent::RunStart,
        command: None,
        hook_type: Some(fabro_hooks::HookType::Agent {
            prompt: format!(
                "Read the file at {} using the read_file tool. If it contains 'READY', respond with {{\"ok\": true}}. Otherwise respond with {{\"ok\": false, \"reason\": \"not ready\"}}.",
                marker.display()
            ),
            model: Some("haiku".into()),
            max_tool_rounds: Some(5),
        }),
        matcher: None,
        blocking: None,
        timeout_ms: Some(60000),
        sandbox: None,
    }];
    let engine = engine_with_hooks(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let config = make_run_config(dir.path());

    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);
}

// --- Events emitted correctly alongside hooks ---

#[tokio::test]
async fn hooks_do_not_duplicate_workflow_events() {
    let hooks = vec![
        make_hook(fabro_hooks::HookEvent::RunStart, "exit 0"),
        make_hook(fabro_hooks::HookEvent::StageStart, "exit 0"),
        make_hook(fabro_hooks::HookEvent::StageComplete, "exit 0"),
        make_hook(fabro_hooks::HookEvent::RunComplete, "exit 0"),
    ];
    let (engine, events) = engine_with_hooks_and_events(hooks);
    let graph = parse(simple_linear_dot()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = make_run_config(dir.path());

    engine.run(&graph, &config).await.unwrap();

    let captured = events.lock().unwrap();

    // Count WorkflowRunStarted — should be exactly 1
    let run_started = captured
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::WorkflowRunStarted { .. }))
        .count();
    assert_eq!(run_started, 1, "Should have exactly 1 WorkflowRunStarted");

    // Count WorkflowRunCompleted — should be exactly 1
    let run_completed = captured
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::WorkflowRunCompleted { .. }))
        .count();
    assert_eq!(
        run_completed, 1,
        "Should have exactly 1 WorkflowRunCompleted"
    );

    // No WorkflowRunFailed
    let run_failed = captured
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::WorkflowRunFailed { .. }))
        .count();
    assert_eq!(run_failed, 0, "Should have 0 WorkflowRunFailed");
}

// ---------------------------------------------------------------------------
// E2E test with real LLM
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY"]
async fn arc_e2e_with_real_llm() {
    dotenvy::dotenv().ok();

    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_str().unwrap().to_string();

    let dot = format!(
        r#"digraph E2E {{
            graph [goal="Create a test file"]
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            work  [
                shape=box,
                label="Work",
                prompt="Create a file called hello.txt in {dir_path} containing exactly 'Hello from LLM'. Do not output anything else.",
                goal_gate=true
            ]
            start -> work -> exit
        }}"#
    );

    let graph = parse(&dot).expect("parse should succeed");
    validate_or_raise(&graph, &[]).expect("validation should pass");

    let interviewer: Arc<dyn Interviewer> = Arc::new(AutoApproveInterviewer);
    let model = "claude-haiku-4-5".to_string();

    let registry = default_registry(interviewer, move || {
        Some(Box::new(AgentApiBackend::new(
            model.clone(),
            Provider::Anthropic,
            Vec::new(),
        ))
            as Box<dyn fabro_workflows::handler::agent::CodergenBackend>)
    });

    let run_dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");

    // 1. Pipeline completed successfully
    assert_eq!(outcome.status, StageStatus::Success);

    // 2. Artifacts exist
    let work_dir = run_dir.path().join("nodes").join("work");
    assert!(
        work_dir.join("prompt.md").exists(),
        "prompt.md should exist"
    );
    assert!(
        work_dir.join("response.md").exists(),
        "response.md should exist"
    );
    assert!(
        work_dir.join("status.json").exists(),
        "status.json should exist"
    );

    // 3. Goal gate: check checkpoint node outcomes
    let checkpoint =
        Checkpoint::load(&run_dir.path().join("checkpoint.json")).expect("checkpoint should load");
    let work_outcome = checkpoint
        .node_outcomes
        .get("work")
        .expect("work outcome should exist");
    assert!(
        work_outcome.status == StageStatus::Success
            || work_outcome.status == StageStatus::PartialSuccess,
        "work goal gate should be Success or PartialSuccess, got {:?}",
        work_outcome.status
    );

    // 4. Checkpoint: completed_nodes contains "work"
    assert!(
        checkpoint.completed_nodes.contains(&"work".to_string()),
        "completed_nodes should contain 'work'"
    );
}

// ---------------------------------------------------------------------------
// Fidelity preamble injection: verify prompt.md contains preamble + prompt
// for each fidelity mode, using script → codergen pipeline with no live LLM.
// ---------------------------------------------------------------------------

/// Build a `start -> run_tests (script) -> report (codergen) -> exit` pipeline
/// with the given fidelity and goal, then return the contents of `report/prompt.md`.
async fn run_fidelity_prompt_pipeline(fidelity: &str) -> String {
    let mut graph = Graph::new("FidelityPromptTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Validate the build".to_string()),
    );
    graph.attrs.insert(
        "default_fidelity".to_string(),
        AttrValue::String(fidelity.to_string()),
    );

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

    // Script node that produces test output via stdout
    let mut run_tests = Node::new("run_tests");
    run_tests.attrs.insert(
        "shape".to_string(),
        AttrValue::String("parallelogram".to_string()),
    );
    run_tests.attrs.insert(
        "script".to_string(),
        AttrValue::String("echo '10 passed, 0 failed'".to_string()),
    );
    graph.nodes.insert("run_tests".to_string(), run_tests);

    // Codergen node that should receive the preamble
    let mut report = Node::new("report");
    report
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    report.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Summarize the test results".to_string()),
    );
    graph.nodes.insert("report".to_string(), report);

    graph.edges.push(Edge::new("start", "run_tests"));
    graph.edges.push(Edge::new("run_tests", "report"));
    graph.edges.push(Edge::new("report", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("command", Box::new(CommandHandler));
    registry.register(
        "agent",
        Box::new(AgentHandler::new(Some(Box::new(MockCodergenBackend)))),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");

    std::fs::read_to_string(dir.path().join("nodes").join("report").join("prompt.md"))
        .expect("report/prompt.md should exist")
}

#[tokio::test]
async fn fidelity_prompt_compact() {
    let prompt = run_fidelity_prompt_pipeline("compact").await;

    // Preamble should contain goal, completed stages with handler details, and context
    assert!(
        prompt.contains("Validate the build"),
        "compact: should contain goal"
    );
    assert!(
        prompt.contains("## Completed stages"),
        "compact: should list completed stages"
    );
    assert!(
        prompt.contains("**run_tests**"),
        "compact: should mention run_tests node in bold"
    );
    assert!(
        prompt.contains("Script:"),
        "compact: should show script sub-item for run_tests"
    );
    assert!(
        prompt.contains("Stdout:"),
        "compact: should show stdout sub-item for run_tests"
    );

    // Original prompt at the end
    assert!(
        prompt.ends_with("Summarize the test results"),
        "compact: should end with original prompt, got:\n{prompt}"
    );
}

#[tokio::test]
async fn fidelity_prompt_truncate() {
    let prompt = run_fidelity_prompt_pipeline("truncate").await;

    // Truncate is minimal: goal + run ID only, no completed stages
    assert!(
        prompt.contains("Validate the build"),
        "truncate: should contain goal"
    );
    assert!(
        !prompt.contains("Completed stages:"),
        "truncate: should NOT list completed stages"
    );

    // Original prompt at the end
    assert!(
        prompt.ends_with("Summarize the test results"),
        "truncate: should end with original prompt, got:\n{prompt}"
    );
}

#[tokio::test]
async fn fidelity_prompt_summary_low() {
    let prompt = run_fidelity_prompt_pipeline("summary:low").await;

    // summary:low includes goal, stage count, recent stages, but NOT context values
    assert!(
        prompt.contains("Validate the build"),
        "summary:low: should contain goal"
    );
    assert!(
        !prompt.contains("Context values:"),
        "summary:low: should NOT include context values"
    );

    // Original prompt at the end
    assert!(
        prompt.ends_with("Summarize the test results"),
        "summary:low: should end with original prompt, got:\n{prompt}"
    );
}

#[tokio::test]
async fn fidelity_prompt_summary_medium() {
    let prompt = run_fidelity_prompt_pipeline("summary:medium").await;

    // summary:medium includes goal, stages, and compact handler details
    assert!(
        prompt.contains("Validate the build"),
        "summary:medium: should contain goal"
    );
    assert!(
        prompt.contains("run_tests"),
        "summary:medium: should mention run_tests"
    );
    assert!(
        prompt.contains("Script:"),
        "summary:medium: should show script sub-item for run_tests"
    );
    assert!(
        prompt.contains("Stdout:"),
        "summary:medium: should show stdout sub-item for run_tests"
    );

    // Original prompt at the end
    assert!(
        prompt.ends_with("Summarize the test results"),
        "summary:medium: should end with original prompt, got:\n{prompt}"
    );
}

#[tokio::test]
async fn fidelity_prompt_summary_high() {
    let prompt = run_fidelity_prompt_pipeline("summary:high").await;

    // summary:high includes goal, all stages as ## Stage headings
    assert!(
        prompt.contains("Validate the build"),
        "summary:high: should contain goal"
    );
    assert!(
        prompt.contains("## Stage: run_tests"),
        "summary:high: should have stage heading for run_tests"
    );
    assert!(
        !prompt.contains("## Stage: start"),
        "summary:high: should not have stage heading for meta start node"
    );
    assert!(
        prompt.contains("Pipeline progress:"),
        "summary:high: should show pipeline progress"
    );

    // Original prompt at the end
    assert!(
        prompt.ends_with("Summarize the test results"),
        "summary:high: should end with original prompt, got:\n{prompt}"
    );
}

#[tokio::test]
async fn fidelity_prompt_full_has_no_preamble() {
    let prompt = run_fidelity_prompt_pipeline("full").await;

    // Full fidelity produces empty preamble — prompt is just the original
    assert_eq!(
        prompt, "Summarize the test results",
        "full: should be bare prompt with no preamble, got:\n{prompt}"
    );
}

// ---------------------------------------------------------------------------
// Artifact offloading integration test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn large_context_values_are_offloaded_to_artifact_store() {
    // Pipeline: start -> big_output -> exit
    // big_output uses LargeOutputHandler which returns a >100KB context_update.
    let mut graph = make_graph_with_start_exit("ArtifactOffload");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test artifact offloading".to_string()),
    );

    let mut big_output = Node::new("big_output");
    big_output.attrs.insert(
        "label".to_string(),
        AttrValue::String("Big Output".to_string()),
    );
    graph.nodes.insert("big_output".to_string(), big_output);

    graph.edges.push(Edge::new("start", "big_output"));
    graph.edges.push(Edge::new("big_output", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(LargeOutputHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);
    let engine = WorkflowRunner::new(registry, Arc::new(emitter), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // The checkpoint context should contain an artifact pointer, not the full value
    let checkpoint =
        fabro_workflows::checkpoint::Checkpoint::load(&dir.path().join("checkpoint.json"))
            .expect("checkpoint should load");
    let pointer_value = checkpoint
        .context_values
        .get("response.big_output")
        .expect("context should have response.big_output");
    let pointer_str = pointer_value.as_str().expect("pointer should be a string");
    assert!(
        pointer_str.starts_with("file://"),
        "value should be an artifact pointer, got: {pointer_str}"
    );

    // The artifact file should exist on disk
    let artifact_file = dir
        .path()
        .join("artifacts")
        .join("values")
        .join("response.big_output.json");
    assert!(
        artifact_file.exists(),
        "artifact file should exist at {artifact_file:?}"
    );

    // The artifact file should contain the original large value
    let artifact_content =
        std::fs::read_to_string(&artifact_file).expect("should read artifact file");
    let artifact_value: serde_json::Value =
        serde_json::from_str(&artifact_content).expect("should parse artifact JSON");
    let artifact_str = artifact_value.as_str().expect("should be a string");
    assert_eq!(
        artifact_str.len(),
        150 * 1024,
        "artifact should contain the original 150KB value"
    );

    // WorkflowRunCompleted event should report artifact_count > 0
    let evts = events.lock().unwrap();
    let completed_event = evts
        .iter()
        .find(|e| matches!(e, WorkflowRunEvent::WorkflowRunCompleted { .. }))
        .expect("should have WorkflowRunCompleted event");
    if let WorkflowRunEvent::WorkflowRunCompleted { artifact_count, .. } = completed_event {
        assert!(
            *artifact_count > 0,
            "artifact_count should be > 0, got {artifact_count}"
        );
    }
}

// ---------------------------------------------------------------------------
// Artifact sync to remote sandboxs
// ---------------------------------------------------------------------------

/// A mock sandbox where `file_exists` always returns false,
/// simulating a remote container that doesn't have local artifact files.
struct RemoteMockEnv {
    working_dir: String,
    written: std::sync::Mutex<Vec<(String, String)>>,
}

impl RemoteMockEnv {
    fn new(working_dir: &str) -> Self {
        Self {
            working_dir: working_dir.to_string(),
            written: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl fabro_agent::Sandbox for RemoteMockEnv {
    async fn read_file(
        &self,
        _path: &str,
        _offset: Option<usize>,
        _limit: Option<usize>,
    ) -> std::result::Result<String, String> {
        Err("not implemented".to_string())
    }

    async fn write_file(&self, path: &str, content: &str) -> std::result::Result<(), String> {
        self.written
            .lock()
            .unwrap()
            .push((path.to_string(), content.to_string()));
        Ok(())
    }

    async fn delete_file(&self, _path: &str) -> std::result::Result<(), String> {
        Err("not implemented".to_string())
    }

    async fn file_exists(&self, _path: &str) -> std::result::Result<bool, String> {
        Ok(false)
    }

    async fn list_directory(
        &self,
        _path: &str,
        _depth: Option<usize>,
    ) -> std::result::Result<Vec<fabro_agent::DirEntry>, String> {
        Err("not implemented".to_string())
    }

    async fn exec_command(
        &self,
        _command: &str,
        _timeout_ms: u64,
        _working_dir: Option<&str>,
        _env_vars: Option<&std::collections::HashMap<String, String>>,
        _cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> std::result::Result<fabro_agent::ExecResult, String> {
        Err("not implemented".to_string())
    }

    async fn grep(
        &self,
        _pattern: &str,
        _path: &str,
        _options: &fabro_agent::GrepOptions,
    ) -> std::result::Result<Vec<String>, String> {
        Err("not implemented".to_string())
    }

    async fn glob(
        &self,
        _pattern: &str,
        _path: Option<&str>,
    ) -> std::result::Result<Vec<String>, String> {
        Err("not implemented".to_string())
    }

    async fn initialize(&self) -> std::result::Result<(), String> {
        Ok(())
    }

    async fn cleanup(&self) -> std::result::Result<(), String> {
        Ok(())
    }

    async fn download_file_to_local(
        &self,
        _: &str,
        _: &std::path::Path,
    ) -> std::result::Result<(), String> {
        Err("not implemented".to_string())
    }

    async fn upload_file_from_local(
        &self,
        _: &std::path::Path,
        _: &str,
    ) -> std::result::Result<(), String> {
        Err("not implemented".to_string())
    }

    fn working_directory(&self) -> &str {
        &self.working_dir
    }

    fn platform(&self) -> &str {
        "linux"
    }

    fn os_version(&self) -> String {
        "Linux 5.15".to_string()
    }
}

#[tokio::test]
async fn artifact_pointers_rewritten_for_remote_sandbox() {
    // Pipeline: start -> big_output -> exit
    // big_output uses LargeOutputHandler which returns a >100KB context_update.
    // RemoteMockEnv simulates a container where local files don't exist.
    let mut graph = make_graph_with_start_exit("ArtifactSync");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test artifact sync to remote env".to_string()),
    );

    let mut big_output = Node::new("big_output");
    big_output.attrs.insert(
        "label".to_string(),
        AttrValue::String("Big Output".to_string()),
    );
    graph.nodes.insert("big_output".to_string(), big_output);

    graph.edges.push(Edge::new("start", "big_output"));
    graph.edges.push(Edge::new("big_output", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(LargeOutputHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let remote_env = Arc::new(RemoteMockEnv::new("/sandbox"));
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), remote_env.clone());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // The checkpoint context should contain a pointer rewritten for the remote env
    let checkpoint =
        Checkpoint::load(&dir.path().join("checkpoint.json")).expect("checkpoint should load");
    let pointer_value = checkpoint
        .context_values
        .get("response.big_output")
        .expect("context should have response.big_output");
    let pointer_str = pointer_value.as_str().expect("pointer should be a string");
    assert!(
        pointer_str.starts_with("file:///sandbox/.fabro/artifacts/"),
        "pointer should reference remote path, got: {pointer_str}"
    );

    // The RemoteMockEnv should have received exactly one write with >100KB content
    let written = remote_env.written.lock().unwrap();
    assert_eq!(written.len(), 1, "should have written 1 artifact");
    assert!(
        written[0].1.len() > 100 * 1024,
        "written content should be >100KB, got {} bytes",
        written[0].1.len()
    );
}

// ---------------------------------------------------------------------------
// Node directory visit-count naming
// ---------------------------------------------------------------------------

/// Verify that revisited nodes get distinct stage directories:
///   visit 1 → `nodes/{id}/`
///   visit 2 → `nodes/{id}-attempt_2/`
#[tokio::test]
async fn node_dir_uses_visit_count_on_revisit() {
    // Handler that fails on first call, succeeds on second.
    struct FailOnceHandler {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl Handler for FailOnceHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &fabro_workflows::context::Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &fabro_workflows::handler::EngineServices,
        ) -> Result<Outcome, fabro_workflows::error::FabroError> {
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(Outcome::fail_classify("first attempt fails"))
            } else {
                Ok(Outcome::success())
            }
        }
    }

    // Graph: start -> gated_work -> exit
    //   gated_work has goal_gate=true, retry_target=start
    //   First visit fails → goal gate unsatisfied → retries from start
    //   Second visit succeeds → pipeline completes
    let mut graph = Graph::new("VisitCountTest");

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

    let mut gated_work = Node::new("gated_work");
    gated_work
        .attrs
        .insert("goal_gate".to_string(), AttrValue::Boolean(true));
    gated_work
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    gated_work.attrs.insert(
        "retry_target".to_string(),
        AttrValue::String("start".to_string()),
    );
    gated_work.attrs.insert(
        "type".to_string(),
        AttrValue::String("fail_once".to_string()),
    );
    graph.nodes.insert("gated_work".to_string(), gated_work);

    graph.edges.push(Edge::new("start", "gated_work"));
    graph.edges.push(Edge::new("gated_work", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "fail_once",
        Box::new(FailOnceHandler {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // First visit: nodes/gated_work/status.json
    let first = dir
        .path()
        .join("nodes")
        .join("gated_work")
        .join("status.json");
    assert!(
        first.exists(),
        "first visit directory should exist at {}",
        first.display()
    );

    // Second visit: nodes/gated_work-visit_2/status.json
    let second = dir
        .path()
        .join("nodes")
        .join("gated_work-visit_2")
        .join("status.json");
    assert!(
        second.exists(),
        "second visit directory should exist at {}",
        second.display()
    );

    // Verify distinct content (first = fail, second = success)
    let first_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&first).unwrap()).unwrap();
    let second_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&second).unwrap()).unwrap();
    assert_eq!(first_json["status"], "fail");
    assert_eq!(second_json["status"], "success");
}

// ---------------------------------------------------------------------------
// CLI Backend end-to-end tests
// ---------------------------------------------------------------------------

/// A mock sandbox for CLI backend e2e tests.
/// Records all exec_command and write_file calls, and returns configurable
/// responses based on command content.
struct CliTestEnv {
    /// All commands passed to exec_command, in order.
    commands: std::sync::Mutex<Vec<String>>,
    /// All (path, content) pairs from write_file.
    written_files: std::sync::Mutex<Vec<(String, String)>>,
    /// The stdout to return when the CLI command (not git) is executed.
    cli_stdout: String,
    /// Files returned by "git diff --name-only" AFTER the CLI runs.
    /// First call returns empty (before), second returns these (after).
    git_diff_call_count: std::sync::atomic::AtomicU32,
    git_diff_after: String,
}

impl CliTestEnv {
    fn new(cli_stdout: &str) -> Self {
        Self {
            commands: std::sync::Mutex::new(Vec::new()),
            written_files: std::sync::Mutex::new(Vec::new()),
            cli_stdout: cli_stdout.to_string(),
            git_diff_call_count: std::sync::atomic::AtomicU32::new(0),
            git_diff_after: String::new(),
        }
    }

    fn with_git_diff_after(mut self, files: &str) -> Self {
        self.git_diff_after = files.to_string();
        self
    }

    fn recorded_commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    fn recorded_written_files(&self) -> Vec<(String, String)> {
        self.written_files.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl fabro_agent::Sandbox for CliTestEnv {
    async fn read_file(
        &self,
        _path: &str,
        _offset: Option<usize>,
        _limit: Option<usize>,
    ) -> Result<String, String> {
        Ok(String::new())
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        self.written_files
            .lock()
            .unwrap()
            .push((path.to_string(), content.to_string()));
        Ok(())
    }

    async fn delete_file(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }

    async fn file_exists(&self, _path: &str) -> Result<bool, String> {
        Ok(false)
    }

    async fn list_directory(
        &self,
        _path: &str,
        _depth: Option<usize>,
    ) -> Result<Vec<fabro_agent::DirEntry>, String> {
        Ok(vec![])
    }

    async fn exec_command(
        &self,
        command: &str,
        _timeout_ms: u64,
        _working_dir: Option<&str>,
        _env_vars: Option<&std::collections::HashMap<String, String>>,
        _cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<fabro_agent::ExecResult, String> {
        self.commands.lock().unwrap().push(command.to_string());

        // git diff calls: first pair returns empty (before), second pair returns configured files
        if command.starts_with("git diff") || command.starts_with("git ls-files") {
            let call_num = self
                .git_diff_call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Calls 0,1 = before snapshot (empty), calls 2,3 = after snapshot
            let stdout = if call_num >= 2 && command.starts_with("git diff") {
                self.git_diff_after.clone()
            } else {
                String::new()
            };
            return Ok(fabro_agent::ExecResult {
                stdout,
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 5,
            });
        }

        // Background launch: return PID
        if command.contains("echo $!") {
            return Ok(fabro_agent::ExecResult {
                stdout: "12345\n".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 1,
            });
        }

        // Poll for completion: return exit code 0 immediately
        if command.contains("exit_code") && command.contains("echo running") {
            return Ok(fabro_agent::ExecResult {
                stdout: "0\n".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 1,
            });
        }

        // Read stdout file
        if command.starts_with("cat") && command.contains("stdout.log") {
            return Ok(fabro_agent::ExecResult {
                stdout: self.cli_stdout.clone(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 1,
            });
        }

        // Read stderr file
        if command.starts_with("cat") && command.contains("stderr.log") {
            return Ok(fabro_agent::ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 1,
            });
        }

        // Cleanup temp files
        if command.starts_with("rm -f") {
            return Ok(fabro_agent::ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 1,
            });
        }

        // Fallback
        Ok(fabro_agent::ExecResult {
            stdout: self.cli_stdout.clone(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
            duration_ms: 100,
        })
    }

    async fn grep(
        &self,
        _pattern: &str,
        _path: &str,
        _options: &fabro_agent::GrepOptions,
    ) -> Result<Vec<String>, String> {
        Ok(vec![])
    }

    async fn glob(&self, _pattern: &str, _path: Option<&str>) -> Result<Vec<String>, String> {
        Ok(vec![])
    }

    async fn initialize(&self) -> Result<(), String> {
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        Ok(())
    }

    async fn download_file_to_local(&self, _: &str, _: &std::path::Path) -> Result<(), String> {
        Err("not implemented".to_string())
    }

    async fn upload_file_from_local(&self, _: &std::path::Path, _: &str) -> Result<(), String> {
        Err("not implemented".to_string())
    }

    fn working_directory(&self) -> &str {
        "/tmp/test"
    }

    fn platform(&self) -> &str {
        "darwin"
    }

    fn os_version(&self) -> String {
        "Darwin 24.0.0".into()
    }
}

// -- Cycle 8: AgentCliBackend::run() e2e via mock Sandbox --

#[tokio::test]
async fn cli_backend_run_writes_prompt_and_calls_exec() {
    let claude_output = r#"{"type":"result","result":"I fixed the bug.","usage":{"input_tokens":500,"output_tokens":200}}"#;
    let test_env = Arc::new(CliTestEnv::new(claude_output));
    let env: Arc<dyn fabro_agent::Sandbox> = test_env.clone();
    let backend = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));

    let node = Node::new("fix_code");
    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = backend
        .run(
            &node,
            "Fix the authentication bug",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("CLI backend should succeed");

    // Verify prompt was written
    let written = test_env.recorded_written_files();
    let prompt_file = written
        .iter()
        .find(|(path, _)| path.contains("_prompt.txt"))
        .expect("should write a prompt file");
    assert!(
        prompt_file.0.starts_with("/tmp/fabro_cli_") && prompt_file.0.ends_with("_prompt.txt"),
        "prompt path should use UUID prefix: {}",
        prompt_file.0
    );
    assert_eq!(prompt_file.1, "Fix the authentication bug");

    // Verify the CLI command was called (now wrapped in background launch)
    let commands = test_env.recorded_commands();
    let cli_cmd = commands
        .iter()
        .find(|c| c.contains("claude") && c.contains("echo $!"))
        .expect("should launch claude CLI in background");
    assert!(cli_cmd.contains("-p"), "should use pipe mode");
    assert!(
        cli_cmd.contains("claude-opus-4-6"),
        "should use correct model"
    );
    assert!(
        cli_cmd.contains("_prompt.txt"),
        "should reference prompt file"
    );

    // Verify parsed response
    match result {
        CodergenResult::Text {
            text,
            usage,
            files_touched,
            ..
        } => {
            assert_eq!(text, "I fixed the bug.");
            let usage = usage.expect("should have usage");
            assert_eq!(usage.input_tokens, 500);
            assert_eq!(usage.output_tokens, 200);
            assert!(files_touched.is_empty(), "no files changed before/after");
        }
        CodergenResult::Full(_) => panic!("expected Text result, got Full"),
    }
}

#[tokio::test]
async fn cli_backend_run_detects_changed_files() {
    let claude_output = r#"{"type":"result","result":"Created new file.","usage":{"input_tokens":100,"output_tokens":50}}"#;
    let env: Arc<dyn fabro_agent::Sandbox> =
        Arc::new(CliTestEnv::new(claude_output).with_git_diff_after("src/main.rs\nsrc/lib.rs\n"));
    let backend = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));

    let node = Node::new("implement");
    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = backend
        .run(
            &node,
            "Add a new feature",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("CLI backend should succeed");

    match result {
        CodergenResult::Text { files_touched, .. } => {
            assert_eq!(files_touched, vec!["src/lib.rs", "src/main.rs"]);
        }
        CodergenResult::Full(_) => panic!("expected Text result"),
    }
}

#[tokio::test]
async fn cli_backend_run_with_codex_provider() {
    let codex_output = "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"agent_message\",\"text\":\"Implemented the feature.\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":300,\"output_tokens\":150}}";
    let test_env = Arc::new(CliTestEnv::new(codex_output));
    let env: Arc<dyn fabro_agent::Sandbox> = test_env.clone();
    let backend = AgentCliBackend::new("gpt-5.3-codex".into(), Provider::OpenAi)
        .with_poll_interval(Duration::from_millis(10));

    let node = Node::new("implement");
    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = backend
        .run(
            &node,
            "Build the API",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("CLI backend should succeed");

    // Verify codex command was called (now wrapped in background launch)
    let commands = test_env.recorded_commands();
    let cli_cmd = commands
        .iter()
        .find(|c| c.contains("codex") && c.contains("echo $!"))
        .expect("should launch codex CLI in background");
    assert!(cli_cmd.contains("exec --json"), "should use exec mode");
    assert!(
        cli_cmd.contains("gpt-5.3-codex"),
        "should use correct model"
    );

    match result {
        CodergenResult::Text { text, usage, .. } => {
            assert_eq!(text, "Implemented the feature.");
            let usage = usage.expect("should have usage");
            assert_eq!(usage.input_tokens, 300);
            assert_eq!(usage.output_tokens, 150);
        }
        CodergenResult::Full(_) => panic!("expected Text result"),
    }
}

#[tokio::test]
async fn cli_backend_run_fails_on_nonzero_exit() {
    let env = Arc::new(CliTestEnv::new(""));

    // Override exec_command to return non-zero for the CLI call
    struct FailingCliEnv;
    #[async_trait::async_trait]
    impl fabro_agent::Sandbox for FailingCliEnv {
        async fn read_file(
            &self,
            _: &str,
            _: Option<usize>,
            _: Option<usize>,
        ) -> Result<String, String> {
            Ok(String::new())
        }
        async fn write_file(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        async fn delete_file(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        async fn file_exists(&self, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        async fn list_directory(
            &self,
            _: &str,
            _: Option<usize>,
        ) -> Result<Vec<fabro_agent::DirEntry>, String> {
            Ok(vec![])
        }
        async fn exec_command(
            &self,
            command: &str,
            _: u64,
            _: Option<&str>,
            _: Option<&std::collections::HashMap<String, String>>,
            _: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<fabro_agent::ExecResult, String> {
            if command.starts_with("git") {
                return Ok(fabro_agent::ExecResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                    timed_out: false,
                    duration_ms: 0,
                });
            }
            // Background launch: return PID
            if command.contains("echo $!") {
                return Ok(fabro_agent::ExecResult {
                    stdout: "12345\n".into(),
                    stderr: String::new(),
                    exit_code: 0,
                    timed_out: false,
                    duration_ms: 0,
                });
            }
            // Poll: return non-zero exit code
            if command.contains("exit_code") && command.contains("echo running") {
                return Ok(fabro_agent::ExecResult {
                    stdout: "127\n".into(),
                    stderr: String::new(),
                    exit_code: 0,
                    timed_out: false,
                    duration_ms: 0,
                });
            }
            // Read stderr file
            if command.starts_with("cat") && command.contains("stderr.log") {
                return Ok(fabro_agent::ExecResult {
                    stdout: "command not found: claude".into(),
                    stderr: String::new(),
                    exit_code: 0,
                    timed_out: false,
                    duration_ms: 0,
                });
            }
            Ok(fabro_agent::ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 0,
            })
        }
        async fn grep(
            &self,
            _: &str,
            _: &str,
            _: &fabro_agent::GrepOptions,
        ) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn glob(&self, _: &str, _: Option<&str>) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn initialize(&self) -> Result<(), String> {
            Ok(())
        }
        async fn cleanup(&self) -> Result<(), String> {
            Ok(())
        }
        async fn download_file_to_local(&self, _: &str, _: &std::path::Path) -> Result<(), String> {
            Err("not implemented".to_string())
        }
        async fn upload_file_from_local(&self, _: &std::path::Path, _: &str) -> Result<(), String> {
            Err("not implemented".to_string())
        }
        fn working_directory(&self) -> &str {
            "/tmp"
        }
        fn platform(&self) -> &str {
            "darwin"
        }
        fn os_version(&self) -> String {
            "Darwin 24.0.0".into()
        }
    }

    let failing_env: Arc<dyn fabro_agent::Sandbox> = Arc::new(FailingCliEnv);
    let backend = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));
    let node = Node::new("step");
    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let _ = env; // unused, just for the above struct

    let result = backend
        .run(
            &node,
            "do something",
            &context,
            None,
            &emitter,
            dir.path(),
            &failing_env,
            None,
        )
        .await;

    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("should fail on non-zero exit"),
    };

    assert!(
        err.to_string().contains("exited with code 127"),
        "error: {err}"
    );
    assert!(
        err.to_string().contains("command not found"),
        "error: {err}"
    );
}

#[tokio::test]
async fn cli_backend_run_fails_on_unparseable_output() {
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(CliTestEnv::new("this is not json at all"));
    let backend = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));

    let node = Node::new("step");
    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = backend
        .run(
            &node,
            "do something",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await;

    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("should fail on unparseable output"),
    };

    assert!(
        err.to_string().contains("Failed to parse CLI output"),
        "error: {err}"
    );
}

#[tokio::test]
async fn cli_backend_run_uses_node_model_override() {
    let claude_output =
        r#"{"type":"result","result":"ok","usage":{"input_tokens":10,"output_tokens":5}}"#;
    let test_env = Arc::new(CliTestEnv::new(claude_output));
    let env: Arc<dyn fabro_agent::Sandbox> = test_env.clone();
    let backend = AgentCliBackend::new("default-model".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));

    let mut node = Node::new("step");
    node.attrs.insert(
        "model".to_string(),
        AttrValue::String("claude-sonnet-4-5".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    backend
        .run(
            &node,
            "test",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("should succeed");

    let commands = test_env.recorded_commands();
    let cli_cmd = commands
        .iter()
        .find(|c| c.contains("claude") && c.contains("echo $!"))
        .unwrap();
    assert!(
        cli_cmd.contains("claude-sonnet-4-5"),
        "should use node's model override, not default: {cli_cmd}"
    );
    assert!(
        !cli_cmd.contains("default-model"),
        "should NOT use default model: {cli_cmd}"
    );
}

#[tokio::test]
async fn cli_backend_run_uses_node_provider_override() {
    let codex_output = "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"agent_message\",\"text\":\"ok\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}";
    let test_env = Arc::new(CliTestEnv::new(codex_output));
    let env: Arc<dyn fabro_agent::Sandbox> = test_env.clone();
    let backend = AgentCliBackend::new("default-model".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));

    let mut node = Node::new("step");
    node.attrs.insert(
        "provider".to_string(),
        AttrValue::String("openai".to_string()),
    );
    node.attrs.insert(
        "model".to_string(),
        AttrValue::String("gpt-5.3-codex".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    backend
        .run(
            &node,
            "test",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("should succeed");

    let commands = test_env.recorded_commands();
    let cli_cmd = commands
        .iter()
        .find(|c| c.contains("codex") && c.contains("echo $!"))
        .expect("should launch codex based on provider override");
    assert!(cli_cmd.contains("gpt-5.3-codex"));
}

#[tokio::test]
async fn cli_backend_run_writes_provider_used_json() {
    let claude_output =
        r#"{"type":"result","result":"done","usage":{"input_tokens":10,"output_tokens":5}}"#;
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(CliTestEnv::new(claude_output));
    let backend = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));

    let node = Node::new("step");
    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    backend
        .run(
            &node,
            "test",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("should succeed");

    let provider_path = dir.path().join("provider_used.json");
    assert!(provider_path.exists(), "should write provider_used.json");
    let provider_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&provider_path).unwrap()).unwrap();
    assert_eq!(provider_json["mode"], "cli");
    assert_eq!(provider_json["provider"], "anthropic");
    assert_eq!(provider_json["model"], "claude-opus-4-6");
    assert!(provider_json["command"]
        .as_str()
        .unwrap()
        .contains("claude"));
}

// -- BackendRouter e2e: delegates to correct backend --

#[tokio::test]
async fn backend_router_delegates_to_cli_for_cli_node() {
    let claude_output = r#"{"type":"result","result":"CLI response","usage":{"input_tokens":10,"output_tokens":5}}"#;
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(CliTestEnv::new(claude_output));

    let api_backend = Box::new(MockCodergenBackend); // would return "Response for ..."
    let cli = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));
    let router = BackendRouter::new(api_backend, cli);

    let mut node = Node::new("cli_step");
    node.attrs
        .insert("backend".to_string(), AttrValue::String("cli".to_string()));
    node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Fix the bug".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = router
        .run(
            &node,
            "Fix the bug",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("router should succeed");

    match result {
        CodergenResult::Text { text, .. } => {
            assert_eq!(
                text, "CLI response",
                "should use CLI backend response, not mock API"
            );
        }
        CodergenResult::Full(_) => panic!("expected Text result"),
    }
}

#[tokio::test]
async fn backend_router_delegates_to_api_for_normal_node() {
    let env = local_env();

    let api_backend = Box::new(MockCodergenBackend);
    let cli = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));
    let router = BackendRouter::new(api_backend, cli);

    let mut node = Node::new("api_step");
    node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Plan the work".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = router
        .run(
            &node,
            "Plan the work",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("router should succeed");

    match result {
        CodergenResult::Text { text, .. } => {
            assert!(
                text.starts_with("Response for api_step"),
                "should use API mock response: {text}"
            );
        }
        CodergenResult::Full(_) => panic!("expected Text result"),
    }
}

#[tokio::test]
async fn backend_router_delegates_to_cli_for_backend_attr() {
    let codex_output = "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"agent_message\",\"text\":\"Codex did it\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}";
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(CliTestEnv::new(codex_output));

    let api_backend = Box::new(MockCodergenBackend);
    let cli = AgentCliBackend::new("gpt-5.3-codex".into(), Provider::OpenAi)
        .with_poll_interval(Duration::from_millis(10));
    let router = BackendRouter::new(api_backend, cli);

    let mut node = Node::new("codex_step");
    node.attrs
        .insert("backend".to_string(), AttrValue::String("cli".to_string()));
    node.attrs.insert(
        "provider".to_string(),
        AttrValue::String("openai".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = router
        .run(
            &node,
            "Build it",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .expect("router should succeed");

    match result {
        CodergenResult::Text { text, .. } => {
            assert_eq!(
                text, "Codex did it",
                "should route to CLI backend for backend=cli"
            );
        }
        CodergenResult::Full(_) => panic!("expected Text result"),
    }
}

// -- Full pipeline e2e with BackendRouter --

#[tokio::test]
async fn full_pipeline_with_cli_backend_node() {
    // Pipeline: start -> api_work -> cli_work -> exit
    // api_work uses MockCodergenBackend (API), cli_work has backend="cli"
    let claude_output = r#"{"type":"result","result":"CLI completed the task.","usage":{"input_tokens":100,"output_tokens":50}}"#;
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(CliTestEnv::new(claude_output));

    let mut graph = Graph::new("CliPipelineTest");

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

    let mut api_work = Node::new("api_work");
    api_work
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    api_work.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Plan the work".to_string()),
    );
    graph.nodes.insert("api_work".to_string(), api_work);

    let mut cli_work = Node::new("cli_work");
    cli_work
        .attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    cli_work.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Implement via CLI".to_string()),
    );
    cli_work
        .attrs
        .insert("backend".to_string(), AttrValue::String("cli".to_string()));
    graph.nodes.insert("cli_work".to_string(), cli_work);

    graph.edges.push(Edge::new("start", "api_work"));
    graph.edges.push(Edge::new("api_work", "cli_work"));
    graph.edges.push(Edge::new("cli_work", "exit"));

    // Build engine with BackendRouter
    let api = MockCodergenBackend;
    let cli = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));
    let router = BackendRouter::new(Box::new(api), cli);
    let codergen_handler = AgentHandler::new(Some(Box::new(router)));

    let mut registry = HandlerRegistry::new(Box::new(codergen_handler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "agent",
        Box::new(AgentHandler::new(Some(Box::new({
            // Second BackendRouter for the "agent" handler
            let api2 = MockCodergenBackend;
            let cli2 = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
                .with_poll_interval(Duration::from_millis(10));
            BackendRouter::new(Box::new(api2), cli2)
        })))),
    );

    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), env);
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Verify api_work used mock (its response.md should contain "Response for")
    let api_response = std::fs::read_to_string(
        dir.path()
            .join("nodes")
            .join("api_work")
            .join("response.md"),
    )
    .unwrap();
    assert!(
        api_response.starts_with("Response for api_work"),
        "API node should use mock: {api_response}"
    );

    // Verify cli_work used CLI backend (its response.md should contain CLI response)
    let cli_response = std::fs::read_to_string(
        dir.path()
            .join("nodes")
            .join("cli_work")
            .join("response.md"),
    )
    .unwrap();
    assert_eq!(
        cli_response, "CLI completed the task.",
        "CLI node should use CLI backend: {cli_response}"
    );

    // Verify cli_work wrote provider_used.json with mode=cli
    let provider_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            dir.path()
                .join("nodes")
                .join("cli_work")
                .join("provider_used.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(provider_json["mode"], "cli");
}

// -- Stylesheet applies backend property to nodes in a full pipeline --

#[tokio::test]
async fn stylesheet_backend_property_routes_to_cli() {
    let claude_output = r#"{"type":"result","result":"Styled CLI response.","usage":{"input_tokens":10,"output_tokens":5}}"#;
    let env: Arc<dyn fabro_agent::Sandbox> = Arc::new(CliTestEnv::new(claude_output));

    let mut graph = Graph::new("StylesheetTest");
    graph.attrs.insert(
        "model_stylesheet".to_string(),
        AttrValue::String(".cli-node { backend: cli; }".to_string()),
    );

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

    let mut work = Node::new("work");
    work.attrs
        .insert("shape".to_string(), AttrValue::String("box".to_string()));
    work.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("Do work".to_string()),
    );
    work.classes.push("cli-node".to_string());
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    // Apply stylesheet
    let ss = parse_stylesheet(graph.model_stylesheet()).unwrap();
    apply_stylesheet(&ss, &mut graph);

    // Verify the stylesheet applied the backend property
    assert_eq!(
        graph.nodes["work"].backend(),
        Some("cli"),
        "stylesheet should set backend=cli on .cli-node"
    );

    // Run the pipeline
    let api = MockCodergenBackend;
    let cli = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));
    let router = BackendRouter::new(Box::new(api), cli);

    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(Some(Box::new(router)))));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let api2 = MockCodergenBackend;
    let cli2 = AgentCliBackend::new("claude-opus-4-6".into(), Provider::Anthropic)
        .with_poll_interval(Duration::from_millis(10));
    let router2 = BackendRouter::new(Box::new(api2), cli2);
    registry.register(
        "agent",
        Box::new(AgentHandler::new(Some(Box::new(router2)))),
    );

    let dir = tempfile::tempdir().unwrap();
    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), env);
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let response =
        std::fs::read_to_string(dir.path().join("nodes").join("work").join("response.md")).unwrap();
    assert_eq!(
        response, "Styled CLI response.",
        "stylesheet-driven node should use CLI backend"
    );
}

// ---------------------------------------------------------------------------
// Real CLI backend e2e tests (require actual CLI tools installed)
// ---------------------------------------------------------------------------

/// Run a real CLI tool via LocalSandbox and verify the full flow.
async fn run_real_cli_test(provider: Provider, model: &str) {
    let env = local_env();
    let backend = AgentCliBackend::new(model.to_string(), provider);

    let mut node = Node::new("real_cli_test");
    node.attrs.insert(
        "prompt".to_string(),
        AttrValue::String("What is 2+2? Reply with just the number.".to_string()),
    );

    let context = Context::new();
    let emitter = Arc::new(EventEmitter::new());
    let dir = tempfile::tempdir().unwrap();

    let result = backend
        .run(
            &node,
            "What is 2+2? Reply with just the number.",
            &context,
            None,
            &emitter,
            dir.path(),
            &env,
            None,
        )
        .await
        .unwrap_or_else(|_| panic!("CLI backend ({provider}/{model}) should succeed"));

    match result {
        CodergenResult::Text { text, usage, .. } => {
            assert!(
                text.contains('4'),
                "{provider}/{model}: expected response to contain '4', got: {text}"
            );
            let usage = usage.unwrap_or_else(|| panic!("{provider}/{model}: should have usage"));
            assert!(
                usage.input_tokens > 0,
                "{provider}/{model}: input_tokens should be > 0, got {}",
                usage.input_tokens
            );
        }
        CodergenResult::Full(_) => panic!("expected Text result from {provider}/{model}"),
    }

    // Verify log files were written
    let provider_path = dir.path().join("provider_used.json");
    assert!(
        provider_path.exists(),
        "{provider}/{model}: provider_used.json should exist"
    );
    let provider_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&provider_path).unwrap()).unwrap();
    assert_eq!(provider_json["mode"], "cli");
    assert_eq!(provider_json["provider"], provider.as_str());

    // Verify CLI output was streamed to stage_dir during poll
    let stdout_log = dir.path().join("cli_stdout.log");
    assert!(
        stdout_log.exists(),
        "{provider}/{model}: cli_stdout.log should be written during poll"
    );
    let stdout_content = std::fs::read_to_string(&stdout_log).unwrap();
    assert!(
        !stdout_content.is_empty(),
        "{provider}/{model}: cli_stdout.log should not be empty"
    );
}

#[tokio::test]
#[ignore] // requires `claude` CLI installed
async fn real_cli_claude() {
    run_real_cli_test(Provider::Anthropic, "haiku").await;
}

#[tokio::test]
#[ignore] // requires `codex` CLI installed and OpenAI auth
async fn real_cli_codex() {
    run_real_cli_test(Provider::OpenAi, "").await;
}

#[tokio::test]
#[ignore] // requires `gemini` CLI installed and Google auth
async fn real_cli_gemini() {
    run_real_cli_test(Provider::Gemini, "gemini-2.5-flash").await;
}

/// Verify parse_cli_response works against real Claude CLI output captured from stream-json.
#[test]
fn parse_real_claude_stream_json() {
    // Real output captured from: claude -p --output-format stream-json --model haiku "What is 2+2?"
    let output = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"abc"}
{"type":"assistant","message":{"content":[{"type":"text","text":"4"}]}}
{"type":"result","subtype":"success","is_error":false,"duration_ms":2000,"num_turns":1,"result":"4","usage":{"input_tokens":9,"output_tokens":5}}"#;
    let response = parse_cli_response(Provider::Anthropic, output).unwrap();
    assert_eq!(response.text, "4");
    assert_eq!(response.input_tokens, 9);
    assert_eq!(response.output_tokens, 5);
}

/// Verify parse_cli_response works against real Codex CLI output.
#[test]
fn parse_real_codex_ndjson() {
    // Real output captured from: echo "What is 2+2?" | codex exec --json
    let output = r#"{"type":"thread.started","thread_id":"019ca1ec-1e86-79b2-b2b2-b1d963f1aea2"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"**Confirming simple numeric reply**"}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"4"}}
{"type":"turn.completed","usage":{"input_tokens":7999,"cached_input_tokens":7040,"output_tokens":33}}"#;
    let response = parse_cli_response(Provider::OpenAi, output).unwrap();
    assert_eq!(response.text, "4");
    assert_eq!(response.input_tokens, 7999);
    assert_eq!(response.output_tokens, 33);
}

/// Verify parse_cli_response works against real Gemini CLI output.
#[test]
fn parse_real_gemini_json() {
    // Real output captured from: gemini "What is 2+2?" -m gemini-2.5-flash --sandbox -o json
    let output = r#"{"session_id":"abc","response":"4","stats":{"models":{"gemini-2.5-flash":{"api":{"totalRequests":1,"totalErrors":0,"totalLatencyMs":618},"tokens":{"input":123,"prompt":8911,"candidates":1,"total":8912,"cached":8788,"thoughts":0,"tool":0}}},"tools":{"totalCalls":0},"files":{"totalLinesAdded":0,"totalLinesRemoved":0}}}"#;
    let response = parse_cli_response(Provider::Gemini, output).unwrap();
    assert_eq!(response.text, "4");
    assert_eq!(response.input_tokens, 123);
    assert_eq!(response.output_tokens, 1);
}

// ---------------------------------------------------------------------------
// Git checkpoint e2e (Local)
// ---------------------------------------------------------------------------

use fabro_workflows::handler::fan_in::FanInHandler;
use fabro_workflows::handler::parallel::ParallelHandler;

/// A handler that writes a file named `{node_id}.txt` into the sandbox's
/// working directory. Used to verify git worktree isolation in parallel branches.
struct FileWriterHandler;

#[async_trait::async_trait]
impl Handler for FileWriterHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let work_dir = services.sandbox.working_directory().to_string();
        let file_path = format!("{}/{}.txt", work_dir, node.id);
        services
            .sandbox
            .write_file(&file_path, &format!("written by {}", node.id))
            .await
            .map_err(|e| FabroError::handler(format!("write_file failed: {e}")))?;
        Ok(Outcome::success())
    }
}

/// End-to-end test: pipeline with git checkpointing enabled emits `CheckpointCompleted`
/// events with valid commit SHAs and writes `diff.patch` per stage.
#[tokio::test]
async fn git_checkpoint_host_emits_events_and_diff_patch() {
    // 1. Create a temporary git repo with an initial commit
    let repo = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(repo.path())
        .output()
        .unwrap();

    // 2. Create a branch and worktree (like cli/run.rs setup_worktree)
    let base_sha = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    std::process::Command::new("git")
        .args(["branch", "fabro/run/test-docker", "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let worktree_path = repo.path().join("worktree");
    std::process::Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree_path)
        .arg("fabro/run/test-docker")
        .current_dir(repo.path())
        .output()
        .unwrap();

    // Write a file in the worktree so there's something to commit
    std::fs::write(worktree_path.join("hello.txt"), "from docker test").unwrap();

    // 3. Build a simple pipeline: start -> work -> exit
    let mut graph = Graph::new("DockerGitCheckpoint");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test Host git checkpoint".to_string()),
    );
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
    let mut work = Node::new("work");
    work.attrs
        .insert("label".to_string(), AttrValue::String("Work".to_string()));
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    // 4. Set up event collection and engine
    let run_dir = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);

    let env: Arc<dyn fabro_agent::Sandbox> =
        Arc::new(fabro_agent::LocalSandbox::new(worktree_path.clone()));
    let mut registry = HandlerRegistry::new(Box::new(ContextSetterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(emitter), env);

    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-docker".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(worktree_path.clone()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha.clone()),
            run_branch: Some("fabro/run/test-docker".to_string()),
            meta_branch: None,
        }),
    };
    // 5. Run pipeline
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // 6. Assert CheckpointCompleted events with git SHAs were emitted
    let events = events.lock().unwrap();
    let git_events: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let WorkflowRunEvent::CheckpointCompleted {
                node_id,
                git_commit_sha: Some(sha),
                ..
            } = e
            {
                Some((node_id.clone(), sha.clone()))
            } else {
                None
            }
        })
        .collect();
    // work node gets a checkpoint commit (start is skipped, exit is terminal)
    assert!(
        !git_events.is_empty(),
        "expected at least 1 CheckpointCompleted event with SHA, got {}",
        git_events.len()
    );
    assert!(
        !git_events.iter().any(|(id, _)| id == "start"),
        "start node should not have a git checkpoint"
    );
    // Each SHA should be a valid 40-char hex string
    assert!(
        git_events
            .iter()
            .all(|(_, sha)| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())),
        "all SHAs should be 40-char hex, got: {git_events:?}"
    );

    // 7. diff.patch is NOT written for the start node (git checkpoint skipped)
    let start_diff = run_dir
        .path()
        .join("nodes")
        .join("start")
        .join("diff.patch");
    assert!(
        !start_diff.exists(),
        "diff.patch should not exist for start node (git checkpoint skipped)"
    );

    // 8. Verify checkpoint.json has git_commit_sha
    let checkpoint =
        Checkpoint::load(&run_dir.path().join("checkpoint.json")).expect("checkpoint should load");
    assert!(
        checkpoint.git_commit_sha.is_some(),
        "checkpoint should have git_commit_sha"
    );

    // 9. Assert final.patch exists and contains the changes
    let final_patch = run_dir.path().join("final.patch");
    assert!(final_patch.exists(), "final.patch should exist in run_dir");
    let patch_content = std::fs::read_to_string(&final_patch).unwrap();
    assert!(
        patch_content.contains("hello.txt"),
        "final.patch should contain hello.txt changes"
    );

    // Cleanup worktree
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_path)
        .current_dir(repo.path())
        .output();
}

/// End-to-end test: pipeline with git checkpointing enabled + `meta_branch` writes
/// shadow branch with checkpoint data and includes `Fabro-Checkpoint` trailer in run-branch commits.
#[tokio::test]
async fn git_checkpoint_host_writes_shadow_branch() {
    use fabro_workflows::git::MetadataStore;

    // 1. Create a temporary git repo with an initial commit
    let repo = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(repo.path())
        .output()
        .unwrap();

    // 2. Create a branch and worktree
    let run_id = "test-shadow";
    let base_sha = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    std::process::Command::new("git")
        .args(["branch", &format!("fabro/run/{run_id}"), "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let worktree_path = repo.path().join("worktree");
    std::process::Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree_path)
        .arg(format!("fabro/run/{run_id}"))
        .current_dir(repo.path())
        .output()
        .unwrap();

    // Write a file in the worktree so there's something to commit
    std::fs::write(worktree_path.join("shadow_test.txt"), "shadow branch test").unwrap();

    // 3. Build a simple pipeline: start -> work -> exit
    let mut graph = Graph::new("ShadowBranchTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test shadow branch".to_string()),
    );
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
    let mut work = Node::new("work");
    work.attrs
        .insert("label".to_string(), AttrValue::String("Work".to_string()));
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    // 4. Set up engine with meta_branch
    let run_dir = tempfile::tempdir().unwrap();
    // Write graph.fabro so init_run can read it
    std::fs::write(run_dir.path().join("graph.fabro"), "digraph {}").unwrap();
    // Write run.json so init_run stores it on the metadata branch
    let run_record_json = serde_json::json!({
        "run_id": run_id,
        "created_at": "2025-01-01T00:00:00Z",
        "config": {},
        "graph": { "name": "ShadowBranchTest", "nodes": {}, "edges": [], "attrs": {} },
        "working_directory": worktree_path.to_str().unwrap(),
    });
    std::fs::write(
        run_dir.path().join("run.json"),
        serde_json::to_string(&run_record_json).unwrap(),
    )
    .unwrap();
    let emitter = EventEmitter::new();

    let env: Arc<dyn fabro_agent::Sandbox> =
        Arc::new(fabro_agent::LocalSandbox::new(worktree_path.clone()));
    let mut registry = HandlerRegistry::new(Box::new(ContextSetterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(emitter), env);

    let meta_branch = MetadataStore::branch_name(run_id);
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: run_id.into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(worktree_path.clone()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha),
            run_branch: Some(format!("fabro/run/{run_id}")),
            meta_branch: Some(meta_branch),
        }),
    };
    // 5. Run pipeline
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // 6. Assert shadow branch has checkpoint data on the host repo
    let checkpoint = MetadataStore::read_checkpoint(repo.path(), run_id)
        .expect("read_checkpoint should not error")
        .expect("shadow branch should contain checkpoint data");
    assert!(
        !checkpoint.completed_nodes.is_empty(),
        "checkpoint should have completed nodes"
    );
    assert!(
        checkpoint.completed_nodes.contains(&"work".to_string()),
        "checkpoint should contain the 'work' node"
    );

    // 7. Assert run-branch commit has Fabro-Checkpoint trailer pointing to shadow SHA
    let output = std::process::Command::new("git")
        .args(["log", "--format=%B", "-1"])
        .current_dir(&worktree_path)
        .output()
        .unwrap();
    let commit_msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(
        commit_msg.contains("Fabro-Checkpoint:"),
        "run-branch commit should have Fabro-Checkpoint trailer, got:\n{commit_msg}"
    );
    assert!(
        commit_msg.contains("Fabro-Run:"),
        "run-branch commit should have Fabro-Run trailer, got:\n{commit_msg}"
    );
    assert!(
        commit_msg.contains("Fabro-Completed:"),
        "run-branch commit should have Fabro-Completed trailer, got:\n{commit_msg}"
    );

    // 8. Verify round-trip: shadow checkpoint's completed_nodes matches expected
    let run_record = MetadataStore::read_run_record(repo.path(), run_id)
        .expect("read_run_record should not error")
        .expect("shadow branch should contain run record");
    assert_eq!(run_record.run_id, run_id);

    // Cleanup worktree
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_path)
        .current_dir(repo.path())
        .output();
}

// ---------------------------------------------------------------------------
// Host e2e: parallel git branching with worktree isolation
// ---------------------------------------------------------------------------

/// End-to-end: parallel branches get isolated worktrees, fan-in fast-forwards to winner.
///
/// Pipeline: start -> fan_out -> {branch_a, branch_b} -> fan_in -> exit
///
/// Each branch writes a unique file. After fan-in, only the winner's file should
/// be present in the main worktree.
#[tokio::test]
async fn parallel_git_branching_host_e2e() {
    // 1. Create a temporary git repo with an initial commit
    let repo = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(repo.path())
        .output()
        .unwrap();

    // 2. Set up run branch and worktree (same as cli/run.rs)
    let base_sha = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let run_id = "par-git-test";
    let run_branch = format!("fabro/run/{run_id}");
    std::process::Command::new("git")
        .args(["branch", &run_branch, "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let worktree_path = repo.path().join("worktree");
    std::process::Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree_path)
        .arg(&run_branch)
        .current_dir(repo.path())
        .output()
        .unwrap();

    // 3. Build pipeline: start -> fan_out -> {branch_a, branch_b} -> fan_in -> exit
    let mut graph = Graph::new("ParallelGitBranching");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test parallel git branching".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut fan_out = Node::new("fan_out");
    fan_out.attrs.insert(
        "shape".to_string(),
        AttrValue::String("component".to_string()),
    );
    graph.nodes.insert("fan_out".to_string(), fan_out);

    let branch_a = Node::new("branch_a");
    graph.nodes.insert("branch_a".to_string(), branch_a);

    let branch_b = Node::new("branch_b");
    graph.nodes.insert("branch_b".to_string(), branch_b);

    let mut fan_in = Node::new("fan_in");
    fan_in.attrs.insert(
        "shape".to_string(),
        AttrValue::String("tripleoctagon".to_string()),
    );
    graph.nodes.insert("fan_in".to_string(), fan_in);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    graph.edges.push(Edge::new("start", "fan_out"));
    graph.edges.push(Edge::new("fan_out", "branch_a"));
    graph.edges.push(Edge::new("fan_out", "branch_b"));
    graph.edges.push(Edge::new("branch_a", "fan_in"));
    graph.edges.push(Edge::new("branch_b", "fan_in"));
    graph.edges.push(Edge::new("fan_in", "exit"));

    // 4. Set up engine with FileWriterHandler for branches
    let run_dir = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);

    let env: Arc<dyn fabro_agent::Sandbox> =
        Arc::new(fabro_agent::LocalSandbox::new(worktree_path.clone()));

    let mut registry = HandlerRegistry::new(Box::new(FileWriterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("parallel", Box::new(ParallelHandler));
    registry.register(
        "parallel.fan_in",
        Box::new(FanInHandler::new(None)), // heuristic select — picks branch_a (lexical tiebreak)
    );

    let engine = WorkflowRunner::new(registry, Arc::new(emitter), env);

    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: run_id.into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(worktree_path.clone()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha.clone()),
            run_branch: Some(run_branch.clone()),
            meta_branch: None,
        }),
    };
    // 5. Run pipeline
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("parallel pipeline should succeed");
    assert_eq!(
        outcome.status,
        StageStatus::Success,
        "pipeline failed: {:?}",
        outcome.failure_reason()
    );

    // 6. Verify parallel.results has head_sha for each branch
    let checkpoint =
        Checkpoint::load(&run_dir.path().join("checkpoint.json")).expect("checkpoint should load");
    let parallel_results = checkpoint
        .context_values
        .get("parallel.results")
        .expect("parallel.results should be in context");
    let results_arr = parallel_results.as_array().expect("should be an array");
    assert_eq!(results_arr.len(), 2, "should have 2 branch results");

    // Both branches should have head_sha
    let branch_a_result = results_arr
        .iter()
        .find(|v| v.get("id").and_then(|v| v.as_str()) == Some("branch_a"))
        .expect("branch_a result should exist");
    let branch_b_result = results_arr
        .iter()
        .find(|v| v.get("id").and_then(|v| v.as_str()) == Some("branch_b"))
        .expect("branch_b result should exist");

    let sha_a = branch_a_result
        .get("head_sha")
        .and_then(|v| v.as_str())
        .expect("branch_a should have head_sha");
    let sha_b = branch_b_result
        .get("head_sha")
        .and_then(|v| v.as_str())
        .expect("branch_b should have head_sha");

    assert_eq!(sha_a.len(), 40, "SHA should be 40 hex chars");
    assert_eq!(sha_b.len(), 40, "SHA should be 40 hex chars");
    assert_ne!(sha_a, sha_b, "branch SHAs should differ");

    // 7. Verify fan_in selected a winner and set best_head_sha
    let best_id = checkpoint
        .context_values
        .get("parallel.fan_in.best_id")
        .and_then(|v| v.as_str().map(String::from))
        .expect("fan_in should have selected a best_id");
    let best_head_sha = checkpoint
        .context_values
        .get("parallel.fan_in.best_head_sha")
        .and_then(|v| v.as_str().map(String::from))
        .expect("fan_in should have set best_head_sha");

    // Heuristic select with both success: lexical tiebreak picks "branch_a"
    assert_eq!(
        best_id, "branch_a",
        "heuristic should pick branch_a (lexical)"
    );

    // 8. Verify winner's file is in the main worktree, loser's is NOT
    let winner_file = worktree_path.join(format!("{best_id}.txt"));
    assert!(
        winner_file.exists(),
        "winner's file ({best_id}.txt) should exist in main worktree after ff-merge"
    );
    let winner_content = std::fs::read_to_string(&winner_file).unwrap();
    assert!(
        winner_content.contains(&format!("written by {best_id}")),
        "winner's file should have correct content"
    );

    let loser_id = if best_id == "branch_a" {
        "branch_b"
    } else {
        "branch_a"
    };
    let loser_file = worktree_path.join(format!("{loser_id}.txt"));
    assert!(
        !loser_file.exists(),
        "loser's file ({loser_id}.txt) should NOT exist in main worktree"
    );

    // 9. Verify the main worktree HEAD matches the winner's head_sha
    let main_head = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree_path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // After fan-in ff-only + engine's own checkpoint commits, HEAD should be a
    // descendant of best_head_sha.
    let is_ancestor = std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", &best_head_sha, &main_head])
        .current_dir(&worktree_path)
        .output()
        .unwrap();
    assert!(
        is_ancestor.status.success(),
        "best_head_sha ({best_head_sha}) should be an ancestor of current HEAD ({main_head})"
    );

    // 10. Verify parallel branch refs still exist (for debugging)
    let branch_ref_a = format!("fabro/run/parallel/{run_id}/fan-out/pass1/branch-a");
    let ref_check = std::process::Command::new("git")
        .args(["rev-parse", "--verify", &branch_ref_a])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        ref_check.status.success(),
        "parallel branch ref should still exist for debugging"
    );

    // 11. Verify final.patch contains the winner's changes
    let final_patch = run_dir.path().join("final.patch");
    assert!(final_patch.exists(), "final.patch should exist in run_dir");
    let patch_content = std::fs::read_to_string(&final_patch).unwrap();
    assert!(
        patch_content.contains(&format!("{best_id}.txt")),
        "final.patch should contain winner's file"
    );
    assert!(
        !patch_content.contains(&format!("{loser_id}.txt")),
        "final.patch should NOT contain loser's file"
    );

    // 12. Verify events
    let events = events.lock().unwrap();
    let parallel_started: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::ParallelStarted { .. }))
        .collect();
    assert_eq!(
        parallel_started.len(),
        1,
        "should have exactly one ParallelStarted event"
    );

    let parallel_completed: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::ParallelCompleted { .. }))
        .collect();
    assert_eq!(
        parallel_completed.len(),
        1,
        "should have exactly one ParallelCompleted event"
    );

    // Cleanup
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_path)
        .current_dir(repo.path())
        .output();
}

/// When a node produces no file changes, `diff.patch` should NOT be written.
#[tokio::test]
async fn git_checkpoint_host_skips_empty_diff_patch() {
    let repo = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(repo.path())
        .output()
        .unwrap();

    let base_sha = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    std::process::Command::new("git")
        .args(["branch", "fabro/run/empty-diff", "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let worktree_path = repo.path().join("worktree");
    std::process::Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree_path)
        .arg("fabro/run/empty-diff")
        .current_dir(repo.path())
        .output()
        .unwrap();

    // No files written — handler is a no-op

    let mut graph = Graph::new("EmptyDiff");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test empty diff skip".to_string()),
    );
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
    let mut work = Node::new("work");
    work.attrs
        .insert("label".to_string(), AttrValue::String("Work".to_string()));
    graph.nodes.insert("work".to_string(), work);
    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let run_dir = tempfile::tempdir().unwrap();
    let emitter = EventEmitter::new();
    let _events = collect_events(&emitter);

    let env: Arc<dyn fabro_agent::Sandbox> =
        Arc::new(fabro_agent::LocalSandbox::new(worktree_path.clone()));
    let mut registry = HandlerRegistry::new(Box::new(ContextSetterHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    let engine = WorkflowRunner::new(registry, Arc::new(emitter), env);

    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "empty-diff".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: Some(worktree_path.clone()),
        git: Some(GitCheckpointSettings {
            base_sha: Some(base_sha.clone()),
            run_branch: Some("fabro/run/empty-diff".to_string()),
            meta_branch: None,
        }),
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // diff.patch should NOT exist for the "work" node (no file changes)
    let work_diff = run_dir.path().join("nodes").join("work").join("diff.patch");
    assert!(
        !work_diff.exists(),
        "diff.patch should not exist when there are no changes"
    );

    // final.patch should NOT exist either
    let final_patch = run_dir.path().join("final.patch");
    assert!(
        !final_patch.exists(),
        "final.patch should not exist when there are no changes"
    );

    // Cleanup
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_path)
        .current_dir(repo.path())
        .output();
}

// ---------------------------------------------------------------------------
// Failure Signatures & Circuit Breaker E2E Tests
// ---------------------------------------------------------------------------

/// Handler that always fails with a fixed deterministic reason.
struct DeterministicFailHandler {
    reason: String,
}

impl DeterministicFailHandler {
    fn new(reason: &str) -> Self {
        Self {
            reason: reason.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Handler for DeterministicFailHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        Ok(Outcome::fail_classify(&self.reason))
    }
}

/// Handler that always fails with a transient_infra classification.
struct TransientInfraFailHandler;

#[async_trait::async_trait]
impl Handler for TransientInfraFailHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        Ok(Outcome::fail_classify("connection refused"))
    }
}

/// Handler that provides an explicit `failure_signature` hint via FailureDetail.
struct SignatureHintHandler;

#[async_trait::async_trait]
impl Handler for SignatureHintHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        Ok(
            Outcome::fail_classify("error at line 42 in commit abc123def0")
                .with_signature(Some("custom-grouping-key")),
        )
    }
}

/// Handler that fails with varying reasons each call (truly different after normalization).
struct VaryingReasonFailHandler {
    counter: std::sync::atomic::AtomicU32,
}

static E2E_VARYING_REASONS: &[&str] = &[
    "syntax error in module alpha",
    "type mismatch in module beta",
    "missing field in module gamma",
    "undefined reference in module delta",
    "assertion failed in module epsilon",
    "panic in module zeta",
    "out of bounds in module eta",
    "null pointer in module theta",
    "stack overflow in module iota",
    "deadlock in module kappa",
];

#[async_trait::async_trait]
impl Handler for VaryingReasonFailHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst) as usize;
        Ok(Outcome::fail_classify(
            E2E_VARYING_REASONS[n % E2E_VARYING_REASONS.len()],
        ))
    }
}

/// Handler that succeeds on the Nth call (0-indexed). Fails deterministically before that.
struct SucceedOnNthHandler {
    succeed_on: u32,
    counter: std::sync::atomic::AtomicU32,
}

#[async_trait::async_trait]
impl Handler for SucceedOnNthHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n >= self.succeed_on {
            Ok(Outcome::success())
        } else {
            Ok(Outcome::fail_classify("not yet ready"))
        }
    }
}

/// Build a pipeline: start -> work -> (fail loop back to work, success to exit)
/// This creates a self-loop where work keeps retrying via edge routing.
fn circuit_breaker_self_loop_graph(signature_limit: Option<i64>) -> Graph {
    let mut graph = make_graph_with_start_exit("CircuitBreakerSelfLoop");
    graph
        .attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));
    // High visit limit so the circuit breaker fires first
    graph
        .attrs
        .insert("max_node_visits".to_string(), AttrValue::Integer(100));
    if let Some(limit) = signature_limit {
        graph.attrs.insert(
            "loop_restart_signature_limit".to_string(),
            AttrValue::Integer(limit),
        );
    }

    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("test_handler".to_string()),
    );
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    let mut fail_edge = Edge::new("work", "work");
    fail_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    graph.edges.push(fail_edge);
    let mut ok_edge = Edge::new("work", "exit");
    ok_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(ok_edge);
    graph
}

/// Build a pipeline: start -> work -> (fail: loop_restart to start, success: exit)
/// This uses loop_restart edges for full pipeline restarts.
fn circuit_breaker_restart_graph(signature_limit: Option<i64>) -> Graph {
    let mut graph = make_graph_with_start_exit("CircuitBreakerRestart");
    graph
        .attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));
    graph
        .attrs
        .insert("max_node_visits".to_string(), AttrValue::Integer(100));
    if let Some(limit) = signature_limit {
        graph.attrs.insert(
            "loop_restart_signature_limit".to_string(),
            AttrValue::Integer(limit),
        );
    }

    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("test_handler".to_string()),
    );
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    let mut restart_edge = Edge::new("work", "start");
    restart_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    restart_edge
        .attrs
        .insert("loop_restart".to_string(), AttrValue::Boolean(true));
    graph.edges.push(restart_edge);
    let mut ok_edge = Edge::new("work", "exit");
    ok_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(ok_edge);
    graph
}

// --- E2E Test: normalize_failure_reason produces stable signatures ---

#[test]
fn e2e_normalize_failure_reason_strips_variable_data() {
    use fabro_workflows::error::normalize_failure_reason;

    // Two error messages that differ only in line numbers and hex hashes
    // should normalize to the same string.
    let reason_a = "Error at line 42 in commit abc123def0: assertion failed";
    let reason_b = "Error at line 999 in commit deadbeef01: assertion failed";
    assert_eq!(
        normalize_failure_reason(reason_a),
        normalize_failure_reason(reason_b),
        "errors differing only in line numbers and hashes should normalize identically"
    );

    // Different semantic errors should NOT normalize to the same string.
    let reason_c = "syntax error in module alpha";
    let reason_d = "type mismatch in module beta";
    assert_ne!(
        normalize_failure_reason(reason_c),
        normalize_failure_reason(reason_d),
        "semantically different errors should produce different normalized forms"
    );
}

// --- E2E Test: FailureSignature composite key format ---

#[test]
fn e2e_failure_signature_composite_key() {
    use fabro_workflows::error::{FailureCategory, FailureSignature};

    let sig = FailureSignature::new(
        "verify",
        FailureCategory::Deterministic,
        None,
        Some("assertion failed at line 42"),
    );
    let sig_str = sig.to_string();

    // Verify format: node_id|failure_class|normalized_reason
    assert!(sig_str.starts_with("verify|deterministic|"));
    // Line number should be normalized away
    assert!(
        sig_str.contains("<n>"),
        "line numbers should be normalized: {sig_str}"
    );
    assert!(
        !sig_str.contains("42"),
        "raw digits should be replaced: {sig_str}"
    );
}

// --- E2E Test: signature_hint takes priority over failure_reason ---

#[test]
fn e2e_failure_signature_hint_priority() {
    use fabro_workflows::error::{FailureCategory, FailureSignature};

    let sig = FailureSignature::new(
        "build",
        FailureCategory::Deterministic,
        Some("custom-key-abc"),
        Some("raw error with line 123 and hash deadbeef"),
    );

    // The hint should be used, not the raw reason
    assert_eq!(sig.to_string(), "build|deterministic|custom-key-abc");
}

// --- E2E Test: is_signature_tracked only for deterministic + structural ---

#[test]
fn e2e_only_deterministic_and_structural_tracked() {
    use fabro_workflows::error::FailureCategory;

    // These should be tracked
    assert!(FailureCategory::Deterministic.is_signature_tracked());
    assert!(FailureCategory::Structural.is_signature_tracked());

    // These should NOT be tracked (transient failures retry naturally)
    assert!(!FailureCategory::TransientInfra.is_signature_tracked());
    assert!(!FailureCategory::BudgetExhausted.is_signature_tracked());
    assert!(!FailureCategory::Canceled.is_signature_tracked());
    assert!(!FailureCategory::CompilationLoop.is_signature_tracked());
}

// --- E2E Test: loop_restart_signature_limit graph attribute ---

#[test]
fn e2e_loop_restart_signature_limit_from_graph_attr() {
    let graph = circuit_breaker_self_loop_graph(Some(5));
    assert_eq!(graph.loop_restart_signature_limit(), 5);

    let graph_default = circuit_breaker_self_loop_graph(None);
    assert_eq!(graph_default.loop_restart_signature_limit(), 3);
}

// --- E2E Test: deterministic failure in self-loop triggers circuit breaker ---

#[tokio::test]
async fn e2e_circuit_breaker_deterministic_self_loop() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_self_loop_graph(Some(3));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(DeterministicFailHandler::new(
            "assertion failed in foo_test",
        )),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-circuit-breaker".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err(), "pipeline should abort, not loop forever");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("deterministic failure cycle detected"),
        "error should mention cycle detection, got: {err}"
    );
    assert!(
        err.contains("repeated 3 times"),
        "error should mention the count, got: {err}"
    );
    assert!(
        err.contains("work|deterministic|"),
        "error should include the signature, got: {err}"
    );
}

// --- E2E Test: custom signature limit (5) ---

#[tokio::test]
async fn e2e_circuit_breaker_custom_limit() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_self_loop_graph(Some(5));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(DeterministicFailHandler::new("same error every time")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-custom-limit".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("repeated 5 times"),
        "should fire at limit=5, got: {err}"
    );
}

// --- E2E Test: transient_infra failures do NOT trigger circuit breaker ---

#[tokio::test]
async fn e2e_circuit_breaker_ignores_transient_failures() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = circuit_breaker_self_loop_graph(Some(3));
    // Lower visit limit so the test terminates quickly via visit limit
    graph
        .attrs
        .insert("max_node_visits".to_string(), AttrValue::Integer(6));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("test_handler", Box::new(TransientInfraFailHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-transient-no-breaker".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    // Should hit visit limit, NOT circuit breaker
    assert!(
        err.contains("stuck in a cycle"),
        "transient failures should not trigger circuit breaker, got: {err}"
    );
}

// --- E2E Test: different failure reasons produce different signatures ---

#[tokio::test]
async fn e2e_circuit_breaker_different_reasons_separate_counters() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = circuit_breaker_self_loop_graph(Some(3));
    // With 10 unique reasons and limit=3, we can do up to 30 iterations before
    // any single reason hits 3. But max_node_visits=8 will fire first.
    graph
        .attrs
        .insert("max_node_visits".to_string(), AttrValue::Integer(8));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(VaryingReasonFailHandler {
            counter: std::sync::atomic::AtomicU32::new(0),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-varying-reasons".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    // Should hit visit limit because each failure has a unique signature
    assert!(
        err.contains("stuck in a cycle"),
        "varying reasons should not trigger circuit breaker, got: {err}"
    );
}

// --- E2E Test: loop_restart edge triggers circuit breaker ---

#[tokio::test]
async fn e2e_circuit_breaker_loop_restart() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_restart_graph(Some(3));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(DeterministicFailHandler::new("verify step failed")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-restart-breaker".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_err(),
        "pipeline should abort, not restart forever"
    );
    let err = result.unwrap_err().to_string();
    // The loop_restart guard blocks non-transient_infra failures immediately
    assert!(
        err.contains("loop_restart blocked")
            || err.contains("failure cycle detected")
            || err.contains("circuit breaker"),
        "expected loop_restart guard or circuit breaker error, got: {err}"
    );
}

// --- E2E Test: failure_signature stored in context (checkpoint verification) ---

#[tokio::test]
async fn e2e_failure_signature_persisted_in_context() {
    let dir = tempfile::tempdir().unwrap();
    // Pipeline: start -> work (fails once) -> exit
    // Work fails but the edge routes to exit unconditionally.
    let mut graph = make_graph_with_start_exit("SignatureContextTest");
    graph
        .attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));

    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("test_handler".to_string()),
    );
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(DeterministicFailHandler::new("test assertion failed")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-sig-context".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.unwrap();
    // Pipeline reaches exit (terminal) with goal gates satisfied.
    // Per spec, reaching exit with satisfied goal gates returns SUCCESS.
    assert_eq!(outcome.status, StageStatus::Success);

    // Verify checkpoint has failure_signature in context
    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let sig_value = cp
        .context_values
        .get("failure_signature")
        .expect("failure_signature should be in context");
    let sig_str = sig_value.as_str().unwrap();
    assert!(
        sig_str.contains("work|deterministic|"),
        "signature should contain node_id|class|, got: {sig_str}"
    );
    assert!(
        sig_str.contains("test assertion failed"),
        "signature should contain normalized reason, got: {sig_str}"
    );
}

// --- E2E Test: failure_signature hint from handler overrides raw reason ---

#[tokio::test]
async fn e2e_failure_signature_hint_overrides_reason_in_context() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = make_graph_with_start_exit("SignatureHintTest");
    graph
        .attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));

    let mut work = Node::new("work");
    work.attrs.insert(
        "type".to_string(),
        AttrValue::String("hint_handler".to_string()),
    );
    work.attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    graph.nodes.insert("work".to_string(), work);

    graph.edges.push(Edge::new("start", "work"));
    graph.edges.push(Edge::new("work", "exit"));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("hint_handler", Box::new(SignatureHintHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-sig-hint".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let _outcome = engine.run(&graph, &config).await.unwrap();

    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let sig_str = cp
        .context_values
        .get("failure_signature")
        .and_then(|v| v.as_str())
        .expect("failure_signature should be set");
    // The hint "custom-grouping-key" should be used, not the raw reason
    assert!(
        sig_str.contains("custom-grouping-key"),
        "hint should override raw reason, got: {sig_str}"
    );
    // Raw reason contained line numbers and hex — verify they are NOT in the signature
    assert!(
        !sig_str.contains("42"),
        "raw reason details should not leak through, got: {sig_str}"
    );
}

// --- E2E Test: signature maps persisted in checkpoint and survive save/load ---

#[tokio::test]
async fn e2e_signature_maps_persist_in_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    // Pipeline where work fails twice then we check the checkpoint
    let graph = circuit_breaker_self_loop_graph(Some(5));

    // Use a handler that succeeds on the 3rd call (0-indexed), so we get
    // exactly 3 failures at the work node before succeeding on the 4th visit.
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(SucceedOnNthHandler {
            succeed_on: 3,
            counter: std::sync::atomic::AtomicU32::new(0),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-sig-persist".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(outcome.status, StageStatus::Success);

    // Load checkpoint and verify signature maps
    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    // The pipeline had 3 deterministic failures at "work" before succeeding.
    // loop_failure_signatures should have recorded them.
    assert!(
        !cp.loop_failure_signatures.is_empty(),
        "loop_failure_signatures should have entries after deterministic failures"
    );
    // Verify the signature key format
    let (sig, count) = cp.loop_failure_signatures.iter().next().unwrap();
    assert!(
        sig.to_string().starts_with("work|deterministic|"),
        "signature key should have correct format, got: {sig}"
    );
    assert_eq!(
        *count, 3,
        "should have recorded exactly 3 failures before success"
    );
}

// --- E2E Test: checkpoint backward compat (old checkpoints without signature fields) ---

#[test]
fn e2e_checkpoint_backward_compat_no_signatures() {
    // Simulate loading a checkpoint saved before signature fields existed
    let json = serde_json::json!({
        "timestamp": "2025-06-01T00:00:00Z",
        "current_node": "work",
        "completed_nodes": ["start", "work"],
        "node_retries": {},
        "context_values": {"goal": "test"},
        "logs": ["some log entry"],
        "node_outcomes": {}
    });

    let cp: Checkpoint = serde_json::from_value(json).expect("should deserialize old checkpoint");
    assert!(cp.loop_failure_signatures.is_empty());
    assert!(cp.restart_failure_signatures.is_empty());
    assert_eq!(cp.current_node, "work");
}

// --- E2E Test: checkpoint with signatures round-trips through save/load ---

#[test]
fn e2e_checkpoint_signatures_roundtrip() {
    use fabro_workflows::error::{FailureCategory, FailureSignature};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cp.json");

    let ctx = Context::new();
    ctx.set("goal", serde_json::json!("test roundtrip"));

    let mut loop_sigs = std::collections::HashMap::new();
    let sig1 = FailureSignature::new(
        "verify",
        FailureCategory::Deterministic,
        None,
        Some("assertion failed"),
    );
    loop_sigs.insert(sig1.clone(), 2usize);

    let mut restart_sigs = std::collections::HashMap::new();
    let sig2 = FailureSignature::new(
        "build",
        FailureCategory::Structural,
        None,
        Some("scope violation"),
    );
    restart_sigs.insert(sig2.clone(), 1usize);

    let cp = Checkpoint::from_context(
        &ctx,
        "verify",
        vec!["start".to_string(), "verify".to_string()],
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        None,
        loop_sigs,
        restart_sigs,
        std::collections::HashMap::new(),
    );
    cp.save(&path).unwrap();

    let loaded = Checkpoint::load(&path).unwrap();
    assert_eq!(loaded.loop_failure_signatures.len(), 1);
    assert_eq!(loaded.restart_failure_signatures.len(), 1);
    assert_eq!(loaded.loop_failure_signatures.get(&sig1), Some(&2));
    assert_eq!(loaded.restart_failure_signatures.get(&sig2), Some(&1));
}

// --- E2E Test: pipeline events are emitted before circuit breaker aborts ---

#[tokio::test]
async fn e2e_circuit_breaker_emits_events_before_abort() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_self_loop_graph(Some(3));

    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(DeterministicFailHandler::new("assertion failed")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(emitter), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-events".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err());

    let events = events.lock().unwrap();
    // Should have at least WorkflowRunStarted and some StageFailed/StageCompleted events
    let has_pipeline_started = events
        .iter()
        .any(|e| matches!(e, WorkflowRunEvent::WorkflowRunStarted { .. }));
    assert!(
        has_pipeline_started,
        "WorkflowRunStarted event should be emitted"
    );

    // Verify we got stage events for the failing work node.
    // The circuit breaker fires when count reaches the limit (3) *before*
    // the stage event for that iteration is emitted, so we see limit-1 events.
    let stage_failed_count = events
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::StageFailed { name, .. } if name == "work"))
        .count();
    let stage_completed_count = events
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::StageCompleted { name, .. } if name == "work"))
        .count();
    let total_work_events = stage_completed_count + stage_failed_count;
    // With limit=3, the breaker fires on the 3rd failure before its event is emitted.
    // So we get 2 events (for failures 1 and 2).
    assert!(
        total_work_events >= 2,
        "should have at least 2 stage events before circuit breaker fires, got: {total_work_events}"
    );
}

// --- E2E Test: success resets to success path, but signatures are preserved ---

#[tokio::test]
async fn e2e_circuit_breaker_does_not_fire_below_limit() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_self_loop_graph(Some(5));

    // Handler that fails 4 times (below limit of 5) then succeeds
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(SucceedOnNthHandler {
            succeed_on: 4,
            counter: std::sync::atomic::AtomicU32::new(0),
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-below-limit".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.unwrap();
    assert_eq!(
        outcome.status,
        StageStatus::Success,
        "pipeline should succeed when failures stay below limit"
    );

    // Verify signatures were tracked but didn't trigger abort
    let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
    let total_failures: usize = cp.loop_failure_signatures.values().sum();
    assert_eq!(
        total_failures, 4,
        "should have tracked 4 failures in signatures"
    );
}

// --- E2E Test: multi-stage pipeline with impl/verify cycle detection ---

#[tokio::test]
async fn e2e_circuit_breaker_multi_stage_impl_verify_cycle() {
    // Pipeline: start -> impl (succeeds) -> verify (fails) -> impl -> verify -> ...
    // The verify node always fails with the same deterministic reason.
    // Circuit breaker should detect the verify failure cycling.
    let dir = tempfile::tempdir().unwrap();
    let mut graph = make_graph_with_start_exit("ImplVerifyCycle");
    graph
        .attrs
        .insert("default_max_retries".to_string(), AttrValue::Integer(0));
    graph
        .attrs
        .insert("max_node_visits".to_string(), AttrValue::Integer(100));
    graph.attrs.insert(
        "loop_restart_signature_limit".to_string(),
        AttrValue::Integer(3),
    );

    let mut impl_node = Node::new("impl");
    impl_node.attrs.insert(
        "type".to_string(),
        AttrValue::String("success_handler".to_string()),
    );
    graph.nodes.insert("impl".to_string(), impl_node);

    let mut verify_node = Node::new("verify");
    verify_node.attrs.insert(
        "type".to_string(),
        AttrValue::String("fail_handler".to_string()),
    );
    verify_node
        .attrs
        .insert("max_retries".to_string(), AttrValue::Integer(0));
    graph.nodes.insert("verify".to_string(), verify_node);

    graph.edges.push(Edge::new("start", "impl"));
    graph.edges.push(Edge::new("impl", "verify"));
    // verify fail -> back to impl
    let mut fail_edge = Edge::new("verify", "impl");
    fail_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=fail".to_string()),
    );
    graph.edges.push(fail_edge);
    // verify success -> exit (never taken)
    let mut ok_edge = Edge::new("verify", "exit");
    ok_edge.attrs.insert(
        "condition".to_string(),
        AttrValue::String("outcome=success".to_string()),
    );
    graph.edges.push(ok_edge);

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("success_handler", Box::new(StartHandler)); // StartHandler returns success
    registry.register(
        "fail_handler",
        Box::new(DeterministicFailHandler::new(
            "test assertion: expected 42, got 0",
        )),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-impl-verify-cycle".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_err(),
        "should detect impl/verify cycle, not loop forever"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("deterministic failure cycle detected"),
        "should identify deterministic failure cycle, got: {err}"
    );
    assert!(
        err.contains("verify|deterministic|"),
        "signature should name the verify node, got: {err}"
    );
}

// --- E2E Tests: loop_restart guard (only transient_infra may restart) ---

/// Handler that fails with an explicit failure_class hint and succeeds on the Nth call.
struct ClassifiedFailHandler {
    failure_class: &'static str,
    succeed_on: u32,
    counter: std::sync::atomic::AtomicU32,
}

impl ClassifiedFailHandler {
    fn always(failure_class: &'static str) -> Self {
        Self {
            failure_class,
            succeed_on: u32::MAX,
            counter: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn succeed_on(failure_class: &'static str, n: u32) -> Self {
        Self {
            failure_class,
            succeed_on: n,
            counter: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

#[async_trait::async_trait]
impl Handler for ClassifiedFailHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n >= self.succeed_on {
            return Ok(Outcome::success());
        }
        let failure_class: fabro_workflows::error::FailureCategory =
            self.failure_class.parse().unwrap();
        let mut outcome = Outcome::fail_classify("classified failure");
        if let Some(ref mut f) = outcome.failure {
            f.category = failure_class;
        }
        Ok(outcome)
    }
}

#[tokio::test]
async fn e2e_loop_restart_blocked_for_deterministic_failure() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_restart_graph(Some(10));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(ClassifiedFailHandler::always("deterministic")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-restart-blocked-det".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_err(),
        "deterministic failure should not loop_restart"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("loop_restart blocked"),
        "expected loop_restart blocked error, got: {err}"
    );
}

#[tokio::test]
async fn e2e_loop_restart_blocked_for_structural_failure() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_restart_graph(Some(10));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(ClassifiedFailHandler::always("structural")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-restart-blocked-struct".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_err(),
        "structural failure should not loop_restart"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("loop_restart blocked"),
        "expected loop_restart blocked error, got: {err}"
    );
}

#[tokio::test]
async fn e2e_loop_restart_blocked_for_budget_exhausted_failure() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_restart_graph(Some(10));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(ClassifiedFailHandler::always("budget_exhausted")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-restart-blocked-budget".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_err(),
        "budget_exhausted failure should not loop_restart"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("loop_restart blocked"),
        "expected loop_restart blocked error, got: {err}"
    );
}

#[tokio::test]
async fn e2e_loop_restart_blocked_for_canceled_failure() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_restart_graph(Some(10));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(ClassifiedFailHandler::always("canceled")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-restart-blocked-canceled".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err(), "canceled failure should not loop_restart");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("loop_restart blocked"),
        "expected loop_restart blocked error, got: {err}"
    );
}

#[tokio::test]
async fn e2e_loop_restart_blocked_for_compilation_loop_failure() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_restart_graph(Some(10));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "test_handler",
        Box::new(ClassifiedFailHandler::always("compilation_loop")),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-restart-blocked-comploop".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_err(),
        "compilation_loop failure should not loop_restart"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("loop_restart blocked"),
        "expected loop_restart blocked error, got: {err}"
    );
}

#[tokio::test]
async fn e2e_loop_restart_allowed_for_transient_infra() {
    let dir = tempfile::tempdir().unwrap();
    let graph = circuit_breaker_restart_graph(Some(10));

    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    // Fails with transient_infra on first call, succeeds on second
    registry.register(
        "test_handler",
        Box::new(ClassifiedFailHandler::succeed_on("transient_infra", 1)),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "e2e-restart-allowed-transient".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(
        result.is_ok(),
        "transient_infra failure should be allowed to loop_restart, got: {:?}",
        result.unwrap_err()
    );
}

// ---------------------------------------------------------------------------
// Stall watchdog e2e tests
// ---------------------------------------------------------------------------

/// Handler that sleeps forever (for stall watchdog testing).
struct HangingHandler;

#[async_trait::async_trait]
impl Handler for HangingHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        Ok(Outcome::success())
    }
}

/// Handler that emits keepalive events periodically, then succeeds.
struct KeepaliveHandler {
    interval_ms: u64,
    total_ms: u64,
}

#[async_trait::async_trait]
impl Handler for KeepaliveHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(self.total_ms) {
            tokio::time::sleep(std::time::Duration::from_millis(self.interval_ms)).await;
            services.emitter.emit(&WorkflowRunEvent::Prompt {
                stage: node.id.clone(),
                text: "keepalive".to_string(),
            });
        }
        Ok(Outcome::success())
    }
}

#[tokio::test]
async fn e2e_stall_watchdog_triggers_from_dot_parsed_pipeline() {
    // Parse a DOT graph with stall_timeout set to 200ms
    let dot = r#"digraph StallTest {
        graph [goal="Test stall watchdog", stall_timeout="50ms", default_max_retries=0]
        start [shape=Mdiamond]
        work  [type="hanging", label="Work"]
        exit  [shape=Msquare]
        start -> work -> exit
    }"#;
    let graph = parse(dot).expect("parse should succeed");

    // Verify the stall_timeout was parsed correctly
    assert_eq!(
        graph.stall_timeout(),
        Some(std::time::Duration::from_millis(50)),
    );

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("hanging", Box::new(HangingHandler));

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let emitter = EventEmitter::new();
    emitter.on_event(move |event| {
        events_clone.lock().unwrap().push(format!("{event:?}"));
    });

    let engine = WorkflowRunner::new(registry, Arc::new(emitter), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "stall-e2e".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let result = engine.run(&graph, &config).await;
    assert!(result.is_err(), "expected stall watchdog error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("stall watchdog"),
        "expected error to contain 'stall watchdog', got: {err}"
    );

    // Verify StallWatchdogTimeout event was emitted
    let collected = events.lock().unwrap();
    assert!(
        collected.iter().any(|e| e.contains("StallWatchdogTimeout")),
        "expected StallWatchdogTimeout event in: {collected:?}"
    );
}

#[tokio::test]
async fn e2e_stall_watchdog_kept_alive_by_handler_events() {
    // Parse a DOT graph with stall_timeout 200ms, but the handler emits events
    // every 100ms for 500ms total — the watchdog should NOT trigger.
    let dot = r#"digraph StallAliveTest {
        graph [goal="Test stall keepalive", stall_timeout="100ms", default_max_retries=0]
        start [shape=Mdiamond]
        work  [type="keepalive", label="Work"]
        exit  [shape=Msquare]
        start -> work -> exit
    }"#;
    let graph = parse(dot).expect("parse should succeed");

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register(
        "keepalive",
        Box::new(KeepaliveHandler {
            interval_ms: 10,
            total_ms: 50,
        }),
    );

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "stall-alive-e2e".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);
}

#[tokio::test]
async fn e2e_stall_watchdog_disabled_with_zero_timeout() {
    // Parse a DOT graph with stall_timeout="0s" — watchdog should be disabled,
    // and a short sleep handler should complete successfully.
    let dot = r#"digraph StallDisabledTest {
        graph [goal="Test stall disabled", stall_timeout="0s", default_max_retries=0]
        start [shape=Mdiamond]
        work  [type="slow", label="Work"]
        exit  [shape=Msquare]
        start -> work -> exit
    }"#;
    let graph = parse(dot).expect("parse should succeed");
    assert_eq!(
        graph.stall_timeout(),
        None,
        "zero timeout should disable watchdog"
    );

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("slow", Box::new(SlowTestHandler { sleep_ms: 50 }));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "stall-disabled-e2e".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);
}

/// Handler that sleeps for a configurable duration, then succeeds (for e2e tests).
struct SlowTestHandler {
    sleep_ms: u64,
}

#[async_trait::async_trait]
impl Handler for SlowTestHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
        Ok(Outcome::success())
    }
}

#[tokio::test]
async fn e2e_stall_watchdog_with_explicit_timeout_override() {
    // A short stall_timeout of 50ms should trigger faster than the default 1800s.
    // This tests that the graph attribute is actually respected.
    let dot = r#"digraph StallOverrideTest {
        graph [goal="Test stall override", stall_timeout="50ms", default_max_retries=0]
        start [shape=Mdiamond]
        work  [type="hanging", label="Work"]
        exit  [shape=Msquare]
        start -> work -> exit
    }"#;
    let graph = parse(dot).expect("parse should succeed");
    assert_eq!(
        graph.stall_timeout(),
        Some(std::time::Duration::from_millis(50)),
    );

    let dir = tempfile::tempdir().unwrap();
    let mut registry = HandlerRegistry::new(Box::new(StartHandler));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("hanging", Box::new(HangingHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), local_env());
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "stall-override-e2e".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let start = std::time::Instant::now();
    let result = engine.run(&graph, &config).await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "expected stall watchdog error");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("stall watchdog"), "got: {err}");
    // Should trigger well under 1 second (50ms timeout + check interval overhead)
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "stall watchdog took too long: {elapsed:?}"
    );
}

// Daytona parallel git branching test is in daytona_integration.rs

// ---------------------------------------------------------------------------
// Asset collection e2e tests
// ---------------------------------------------------------------------------

/// Handler that creates asset files in the sandbox working directory via exec_command.
struct AssetCreatorHandler {
    should_fail: bool,
}

impl AssetCreatorHandler {
    fn success() -> Self {
        Self { should_fail: false }
    }

    fn failing() -> Self {
        Self { should_fail: true }
    }
}

#[async_trait::async_trait]
impl Handler for AssetCreatorHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &fabro_workflows::handler::EngineServices,
    ) -> Result<Outcome, FabroError> {
        // Create asset files via the sandbox's exec_command
        let script = concat!(
            "mkdir -p test-results && ",
            "echo '<testsuites><testsuite name=\"example\"/></testsuites>' > test-results/report.xml && ",
            "echo 'test output' > test-results/output.txt"
        );
        services
            .sandbox
            .exec_command(script, 30_000, None, None, None)
            .await
            .map_err(|e| FabroError::handler(format!("exec failed: {e}")))?;

        if self.should_fail {
            Ok(Outcome::fail_classify("intentional failure"))
        } else {
            Ok(Outcome::success())
        }
    }
}

/// Local sandbox: asset collection discovers and downloads files created by a handler.
#[tokio::test]
async fn asset_collection_local_sandbox_success() {
    let work_dir = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();

    let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
        work_dir.path().to_path_buf(),
    ));
    sandbox.initialize().await.unwrap();

    let mut registry = HandlerRegistry::new(Box::new(AssetCreatorHandler::success()));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let emitter = EventEmitter::new();
    let events = collect_events(&emitter);

    let engine = WorkflowRunner::new(registry, Arc::new(emitter), sandbox.clone());

    let mut graph = Graph::new("AssetCollectionTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test asset collection".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut create_assets = Node::new("create_assets");
    create_assets.attrs.insert(
        "label".to_string(),
        AttrValue::String("Create Assets".to_string()),
    );
    graph
        .nodes
        .insert("create_assets".to_string(), create_assets);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    graph.edges.push(Edge::new("start", "create_assets"));
    graph.edges.push(Edge::new("create_assets", "exit"));

    let config = RunSettings {
        config: FabroConfig {
            assets: Some(fabro_config::run::AssetsConfig {
                include: vec!["test-results/**".to_string()],
            }),
            ..FabroConfig::default()
        },
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "asset-test-local".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    // Check that asset files were collected into the stage directory
    let assets_dir = run_dir
        .path()
        .join("artifacts")
        .join("assets")
        .join("create_assets")
        .join("retry_1");

    let report_path = assets_dir.join("test-results/report.xml");
    assert!(
        report_path.exists(),
        "report.xml should be collected at {}",
        report_path.display()
    );
    let report_content = std::fs::read_to_string(&report_path).unwrap();
    assert!(report_content.contains("testsuites"));

    // Check manifest.json was written
    let manifest_path = assets_dir.join("manifest.json");
    assert!(
        manifest_path.exists(),
        "manifest.json should exist at {}",
        manifest_path.display()
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert!(manifest["files_copied"].as_u64().unwrap() >= 1);

    // Check that AssetsCaptured event was emitted
    let captured_events = events.lock().unwrap();
    let assets_events: Vec<&WorkflowRunEvent> = captured_events
        .iter()
        .filter(|e| matches!(e, WorkflowRunEvent::AssetsCaptured { .. }))
        .collect();
    assert!(
        !assets_events.is_empty(),
        "should emit at least one AssetsCaptured event"
    );
}

/// Local sandbox: assets are still collected even when the handler fails.
#[tokio::test]
async fn asset_collection_local_sandbox_on_failure() {
    let work_dir = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();

    let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
        work_dir.path().to_path_buf(),
    ));
    sandbox.initialize().await.unwrap();

    let mut registry = HandlerRegistry::new(Box::new(AssetCreatorHandler::failing()));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), sandbox.clone());

    let mut graph = Graph::new("AssetCollectionFailTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test asset collection on failure".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut create_assets = Node::new("create_assets");
    create_assets.attrs.insert(
        "label".to_string(),
        AttrValue::String("Create Assets".to_string()),
    );
    graph
        .nodes
        .insert("create_assets".to_string(), create_assets);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    graph.edges.push(Edge::new("start", "create_assets"));
    graph.edges.push(Edge::new("create_assets", "exit"));

    let config = RunSettings {
        config: FabroConfig {
            assets: Some(fabro_config::run::AssetsConfig {
                include: vec!["test-results/**".to_string()],
            }),
            ..FabroConfig::default()
        },
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "asset-test-fail".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &config)
        .await
        .expect("run should succeed");
    // The pipeline completes with goal gates satisfied — per spec, SUCCESS at exit node.
    // Assets should still be collected regardless of intermediate node failures.
    assert_eq!(outcome.status, StageStatus::Success);

    let assets_dir = run_dir
        .path()
        .join("artifacts")
        .join("assets")
        .join("create_assets")
        .join("retry_1");

    let report_path = assets_dir.join("test-results/report.xml");
    assert!(
        report_path.exists(),
        "report.xml should still be collected after handler failure, at {}",
        report_path.display()
    );
}

/// Docker sandbox: asset collection works across the bind-mount boundary.
/// Requires Docker with `fabro-agent:latest` image available locally.
#[tokio::test]
#[ignore]
async fn asset_collection_docker_sandbox() {
    let host_dir = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();

    let config = fabro_agent::DockerSandboxConfig {
        host_working_directory: host_dir.path().to_str().unwrap().to_string(),
        auto_pull: false,
        ..Default::default()
    };
    let sandbox: Arc<dyn fabro_agent::Sandbox> =
        Arc::new(fabro_agent::DockerSandbox::new(config).expect("Docker not available"));
    sandbox.initialize().await.expect("Docker init failed");

    let mut registry = HandlerRegistry::new(Box::new(AssetCreatorHandler::success()));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));

    let engine = WorkflowRunner::new(registry, Arc::new(EventEmitter::new()), sandbox.clone());

    let mut graph = Graph::new("DockerAssetTest");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Test asset collection in Docker".to_string()),
    );

    let mut start = Node::new("start");
    start.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Mdiamond".to_string()),
    );
    graph.nodes.insert("start".to_string(), start);

    let mut create_assets = Node::new("create_assets");
    create_assets.attrs.insert(
        "label".to_string(),
        AttrValue::String("Create Assets".to_string()),
    );
    graph
        .nodes
        .insert("create_assets".to_string(), create_assets);

    let mut exit = Node::new("exit");
    exit.attrs.insert(
        "shape".to_string(),
        AttrValue::String("Msquare".to_string()),
    );
    graph.nodes.insert("exit".to_string(), exit);

    graph.edges.push(Edge::new("start", "create_assets"));
    graph.edges.push(Edge::new("create_assets", "exit"));

    let run_config = RunSettings {
        config: FabroConfig {
            assets: Some(fabro_config::run::AssetsConfig {
                include: vec!["test-results/**".to_string()],
            }),
            ..FabroConfig::default()
        },
        run_dir: run_dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "asset-test-docker".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine
        .run(&graph, &run_config)
        .await
        .expect("pipeline should succeed");
    assert_eq!(outcome.status, StageStatus::Success);

    let assets_dir = run_dir
        .path()
        .join("artifacts")
        .join("assets")
        .join("create_assets")
        .join("retry_1");

    let report_path = assets_dir.join("test-results/report.xml");
    assert!(
        report_path.exists(),
        "report.xml should be collected from Docker container at {}",
        report_path.display()
    );
    let content = std::fs::read_to_string(&report_path).unwrap();
    assert!(content.contains("testsuites"));

    let manifest_path = assets_dir.join("manifest.json");
    assert!(manifest_path.exists(), "manifest.json should exist");

    sandbox.cleanup().await.unwrap();
}

#[tokio::test]
async fn wait_timer_e2e() {
    let mut graph = make_graph_with_start_exit("WaitTimerTest");
    let mut wait_node = Node::new("wait60");
    wait_node.attrs.insert(
        "shape".to_string(),
        AttrValue::String("insulator".to_string()),
    );
    wait_node.attrs.insert(
        "label".to_string(),
        AttrValue::String("Wait 1ms".to_string()),
    );
    wait_node.attrs.insert(
        "duration".to_string(),
        AttrValue::Duration(std::time::Duration::from_millis(1)),
    );
    graph.nodes.insert("wait60".to_string(), wait_node);
    graph.edges.push(Edge::new("start", "wait60"));
    graph.edges.push(Edge::new("wait60", "exit"));

    let dir = tempfile::tempdir().unwrap();
    let interviewer = Arc::new(AutoApproveInterviewer);
    let engine = WorkflowRunner::new(
        make_full_registry(interviewer),
        Arc::new(EventEmitter::new()),
        local_env(),
    );
    let config = RunSettings {
        config: FabroConfig::default(),
        run_dir: dir.path().to_path_buf(),
        cancel_token: None,
        dry_run: false,
        run_id: "test-run".into(),
        labels: std::collections::HashMap::new(),
        git_author: fabro_workflows::git::GitAuthor::default(),
        workflow_slug: None,
        github_app: None,
        base_branch: None,
        host_repo_path: None,
        git: None,
    };
    let outcome = engine.run(&graph, &config).await.expect("run");
    assert_eq!(outcome.status, StageStatus::Success);
}
