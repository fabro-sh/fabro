use std::collections::HashMap;
use std::sync::Arc;

use fabro_agent::Sandbox;
use fabro_graphviz::graph::Graph as GvGraph;

use crate::error::Result;
use crate::event::EventEmitter;
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use crate::pipeline;
use crate::pipeline::types::Initialized;
use crate::records::Checkpoint;
use crate::run_options::RunOptions;

struct InitializedOptions {
    hook_runner: Option<Arc<fabro_hooks::HookRunner>>,
    env: HashMap<String, String>,
    checkpoint: Option<Checkpoint>,
}

fn initialized(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    options: InitializedOptions,
) -> Initialized {
    std::fs::create_dir_all(&run_options.run_dir).expect("failed to create run dir");
    Initialized {
        graph: graph.clone(),
        source: String::new(),
        run_options: run_options.clone(),
        checkpoint: options.checkpoint,
        seed_context: None,
        emitter,
        sandbox,
        registry: Arc::new(registry),
        on_node: None,
        hook_runner: options.hook_runner,
        env: options.env,
        dry_run: run_options.dry_run_enabled(),
        llm_client: None,
        model: String::new(),
        provider: fabro_llm::Provider::Anthropic,
    }
}

pub async fn run_graph(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
) -> Result<Outcome> {
    let executed = pipeline::execute(initialized(
        registry,
        emitter,
        sandbox,
        graph,
        run_options,
        InitializedOptions {
            hook_runner: None,
            env: HashMap::new(),
            checkpoint: None,
        },
    ))
    .await;
    executed.outcome
}

pub async fn run_graph_with_hooks(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    hook_runner: Arc<fabro_hooks::HookRunner>,
    env: Option<HashMap<String, String>>,
) -> Result<Outcome> {
    let executed = pipeline::execute(initialized(
        registry,
        emitter,
        sandbox,
        graph,
        run_options,
        InitializedOptions {
            hook_runner: Some(hook_runner),
            env: env.unwrap_or_default(),
            checkpoint: None,
        },
    ))
    .await;
    executed.outcome
}

pub async fn run_graph_from_checkpoint(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    checkpoint: &Checkpoint,
) -> Result<Outcome> {
    let executed = pipeline::execute(initialized(
        registry,
        emitter,
        sandbox,
        graph,
        run_options,
        InitializedOptions {
            hook_runner: None,
            env: HashMap::new(),
            checkpoint: Some(checkpoint.clone()),
        },
    ))
    .await;
    executed.outcome
}

pub struct WorkflowRunner {
    registry: std::sync::Mutex<Option<HandlerRegistry>>,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
}

impl WorkflowRunner {
    #[must_use]
    pub fn new(
        registry: HandlerRegistry,
        emitter: Arc<EventEmitter>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            registry: std::sync::Mutex::new(Some(registry)),
            emitter,
            sandbox,
        }
    }

    pub async fn run(&self, graph: &GvGraph, run_options: &RunOptions) -> Result<Outcome> {
        let registry = self
            .registry
            .lock()
            .unwrap()
            .take()
            .expect("WorkflowRunner may only be used once");
        run_graph(
            registry,
            Arc::clone(&self.emitter),
            Arc::clone(&self.sandbox),
            graph,
            run_options,
        )
        .await
    }

    pub async fn run_from_checkpoint(
        &self,
        graph: &GvGraph,
        run_options: &RunOptions,
        checkpoint: &Checkpoint,
    ) -> Result<Outcome> {
        let registry = self
            .registry
            .lock()
            .unwrap()
            .take()
            .expect("WorkflowRunner may only be used once");
        run_graph_from_checkpoint(
            registry,
            Arc::clone(&self.emitter),
            Arc::clone(&self.sandbox),
            graph,
            run_options,
            checkpoint,
        )
        .await
    }
}
