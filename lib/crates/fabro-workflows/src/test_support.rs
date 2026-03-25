use std::collections::HashMap;
use std::sync::Arc;

use fabro_agent::Sandbox;

use crate::checkpoint::Checkpoint;
use crate::engine::WorkflowRunEngine;
use crate::error::Result;
use crate::event::EventEmitter;
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use crate::run_settings::RunSettings;

pub async fn run_graph(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &fabro_graphviz::graph::Graph,
    settings: &RunSettings,
) -> Result<Outcome> {
    let engine = WorkflowRunEngine::new(registry, emitter, sandbox);
    engine.run(graph, settings).await
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
    let mut engine = WorkflowRunEngine::new(registry, emitter, sandbox);
    engine.set_hook_runner(hook_runner);
    if let Some(env) = env {
        engine.set_env(env);
    }
    engine.run(graph, settings).await
}

pub async fn run_graph_from_checkpoint(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &fabro_graphviz::graph::Graph,
    settings: &RunSettings,
    checkpoint: &Checkpoint,
) -> Result<Outcome> {
    let engine = WorkflowRunEngine::new(registry, emitter, sandbox);
    engine
        .run_from_checkpoint(graph, settings, checkpoint)
        .await
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
