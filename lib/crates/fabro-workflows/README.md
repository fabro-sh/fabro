# fabro-workflows

A DOT-based pipeline runner for multi-stage AI workflows. Define workflows as Graphviz `digraph` files and execute them with pluggable handlers, conditional routing, human-in-the-loop gates, parallel branching, retry policies, and checkpoint-based recovery.

## Key Concepts

- **Graph** -- A directed graph parsed from DOT syntax containing nodes, edges, and attributes. The graph carries a `goal` describing the pipeline's purpose.
- **Node** -- A workflow step. Graphviz shapes map to handler types (e.g., `Mdiamond` = start, `Msquare` = exit, `box` = agent, `tab` = prompt, `diamond` = conditional, `hexagon` = human gate, `component` = parallel).
- **Edge** -- A connection between nodes with optional `condition`, `label`, `weight`, and `fidelity` attributes that control routing.
- **Handler** -- An async trait implementation that executes a node and returns an `Outcome`. Built-in handlers include `StartHandler`, `ExitHandler`, `AgentHandler`, `PromptHandler`, `ConditionalHandler`, `HumanHandler`, `ParallelHandler`, `FanInHandler`, `CommandHandler`, and `SubWorkflowHandler`.
- **Outcome** -- The result of executing a handler, carrying a `StageStatus` (Success, Fail, PartialSuccess, Retry, Skipped), optional routing hints (`preferred_label`, `suggested_next_ids`), and context updates.
- **Context** -- A thread-safe key-value store shared across pipeline stages, supporting snapshots and isolated cloning for parallel branches.
- **Interviewer** -- A trait for human-in-the-loop interactions. Implementations include `AutoApproveInterviewer`, `QueueInterviewer`, `CallbackInterviewer`, `ConsoleInterviewer`, and `RecordingInterviewer`.
- **Checkpoint** -- A serializable snapshot of execution state (completed nodes, context values) for crash recovery and resume.

## Pipeline Definition

Pipelines are defined using Graphviz DOT syntax:

```dot
digraph MyPipeline {
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
    gate -> implement [label="No", condition="outcome!=success"]
}
```

## Usage

### Parsing and Validating a Pipeline

```rust
use arc_workflows::pipeline::prepare_pipeline;

let dot_source = r#"digraph Simple {
    graph [goal="Run tests"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [shape=box, prompt="Run the test suite"]
    start -> work -> exit
}"#;

let graph = prepare_pipeline(dot_source)
    .expect("pipeline should parse and validate");
assert_eq!(graph.name, "Simple");
assert_eq!(graph.goal(), "Run tests");
```

`prepare_pipeline` parses the DOT source, applies built-in transforms (variable expansion, stylesheet application, preamble injection), and validates the graph against 14 built-in lint rules.

### Running a Pipeline

```rust
use arc_workflows::engine::{PipelineEngine, RunSettings};
use arc_workflows::event::EventEmitter;
use arc_workflows::handler::HandlerRegistry;
use arc_workflows::handler::start::StartHandler;
use arc_workflows::handler::exit::ExitHandler;
use arc_workflows::handler::agent::AgentHandler;
use arc_workflows::pipeline::prepare_pipeline;

let graph = prepare_pipeline(dot_source).unwrap();

let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(None)));
registry.register("start", Box::new(StartHandler));
registry.register("exit", Box::new(ExitHandler));
registry.register("agent", Box::new(AgentHandler::new(None)));

let engine = PipelineEngine::new(registry, EventEmitter::new());
let config = RunSettings {
    config: fabro_config::FabroConfig::default(),
    run_dir: "/tmp/pipeline-run".into(),
    cancel_token: None,
    dry_run: false,
    run_id: "example-run".into(),
    labels: std::collections::HashMap::new(),
    git_author: fabro_workflows::git::GitAuthor::default(),
    workflow_slug: None,
    github_app: None,
    base_branch: None,
    host_repo_path: None,
    git: None,
};

// engine.run(&graph, &config).await
```

### Custom Handlers

Implement the `Handler` trait to add custom node behavior:

```rust
use arc_workflows::handler::Handler;
use arc_workflows::context::Context;
use arc_workflows::graph::{Graph, Node};
use arc_workflows::outcome::Outcome;
use arc_workflows::error::ArcError;
use async_trait::async_trait;
use std::path::Path;

struct MyHandler;

#[async_trait]
impl Handler for MyHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        run_dir: &Path,
    ) -> Result<Outcome, ArcError> {
        // Custom logic here
        Ok(Outcome::success())
    }
}
```

### Model Stylesheets

CSS-like stylesheets control LLM model assignment with specificity-based cascading:

```dot
digraph Styled {
    graph [
        goal="Build feature",
        model_stylesheet="
            * { model: claude-sonnet-4-5;}
            .code { model: claude-opus-4-6; }
            #critical_review { model: gpt-5.2;}
        "
    ]
    // ...
}
```

Selectors by specificity: `*` (universal, 0) < `shape` (1) < `.class` (2) < `#id` (3). Explicit node attributes are never overridden.

### Condition Expressions

Edge conditions use a simple expression syntax for routing:

```
outcome=success
outcome!=fail
outcome=success && context.tests_passed=true
my_flag
```

Clauses support `=`, `!=`, and bare key truthiness checks, joined with `&&`.

### Human-in-the-Loop Gates

Nodes with `shape=hexagon` or `type="human"` pause execution for human input. Outgoing edge labels become selectable options, with accelerator key parsing for patterns like `[A] Approve` and `F) Fix`.

### Parallel Execution

Nodes with `shape=component` fan out to branches concurrently. Configurable join policies: `wait_all` (default), `first_success`.

### Checkpoints and Resume

The engine saves a checkpoint after each node. Resume from a checkpoint with `engine.run_from_checkpoint(&graph, &config, &checkpoint)`.

## Architecture

```
parser (DOT -> AST -> Graph)
  -> transform (variable expansion, stylesheet, preamble)
    -> validation (14 lint rules)
      -> engine (execution loop with retry, edge selection, goal gates)
        -> handler (pluggable node executors)
          -> interviewer (human-in-the-loop I/O)
```
