use std::collections::HashMap;
use std::sync::Arc;

use fabro_agent::Sandbox;

use crate::checkpoint::Checkpoint;
use crate::error::Result;
use crate::event::EventEmitter;
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use crate::pipeline;
use crate::pipeline::types::Initialized;
use crate::run_settings::RunSettings;

struct InitializedOptions {
    hook_runner: Option<Arc<fabro_hooks::HookRunner>>,
    env: HashMap<String, String>,
    checkpoint: Option<Checkpoint>,
}

fn initialized(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &fabro_graphviz::graph::Graph,
    settings: &RunSettings,
    options: InitializedOptions,
) -> Initialized {
    std::fs::create_dir_all(&settings.run_dir).expect("failed to create run dir");
    Initialized {
        graph: graph.clone(),
        source: String::new(),
        settings: settings.clone(),
        checkpoint: options.checkpoint,
        seed_context: None,
        emitter,
        sandbox,
        registry: Arc::new(registry),
        hook_runner: options.hook_runner,
        env: options.env,
        dry_run: settings.dry_run,
    }
}

pub async fn run_graph(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &fabro_graphviz::graph::Graph,
    settings: &RunSettings,
) -> Result<Outcome> {
    let executed = pipeline::execute(initialized(
        registry,
        emitter,
        sandbox,
        graph,
        settings,
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
    graph: &fabro_graphviz::graph::Graph,
    settings: &RunSettings,
    hook_runner: Arc<fabro_hooks::HookRunner>,
    env: Option<HashMap<String, String>>,
) -> Result<Outcome> {
    let executed = pipeline::execute(initialized(
        registry,
        emitter,
        sandbox,
        graph,
        settings,
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
    graph: &fabro_graphviz::graph::Graph,
    settings: &RunSettings,
    checkpoint: &Checkpoint,
) -> Result<Outcome> {
    let executed = pipeline::execute(initialized(
        registry,
        emitter,
        sandbox,
        graph,
        settings,
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

    pub async fn run(
        &self,
        graph: &fabro_graphviz::graph::Graph,
        settings: &RunSettings,
    ) -> Result<Outcome> {
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
            settings,
        )
        .await
    }

    pub async fn run_from_checkpoint(
        &self,
        graph: &fabro_graphviz::graph::Graph,
        settings: &RunSettings,
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
            settings,
            checkpoint,
        )
        .await
    }
}
