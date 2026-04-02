use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use fabro_agent::Sandbox;
use fabro_graphviz::graph::Graph as GvGraph;
use fabro_store::{InMemoryStore, RunStore, Store};
use fabro_types::run::RunRecord;

use crate::error::Result;
use crate::event::EventEmitter;
use crate::git::scan_node_files_from_store;
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use crate::pipeline;
use crate::pipeline::types::Initialized;
use crate::records::{Checkpoint, CheckpointExt};
use crate::run_options::RunOptions;

struct InitializedOptions {
    hook_runner: Option<Arc<fabro_hooks::HookRunner>>,
    env: HashMap<String, String>,
    checkpoint: Option<Checkpoint>,
}

fn bound_emitter(run_id: fabro_types::RunId, observer: &Arc<EventEmitter>) -> Arc<EventEmitter> {
    let emitter = Arc::new(EventEmitter::new(run_id));
    let observer_clone = Arc::clone(observer);
    emitter.on_event(move |event| observer_clone.dispatch_envelope(event));
    emitter
}

async fn initialized(
    registry: HandlerRegistry,
    emitter: Arc<EventEmitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    options: InitializedOptions,
) -> Initialized {
    std::fs::create_dir_all(&run_options.run_dir).expect("failed to create run dir");
    let created_at = Utc::now();
    let inner_store = InMemoryStore::default()
        .create_run(
            &run_options.run_id,
            created_at,
            Some(run_options.run_dir.to_string_lossy().as_ref()),
        )
        .await
        .expect("failed to create in-memory run store");
    let run_store = inner_store;
    run_store
        .put_run(&RunRecord {
            run_id: run_options.run_id,
            created_at,
            settings: run_options.settings.clone(),
            graph: graph.clone(),
            workflow_slug: run_options.workflow_slug.clone(),
            working_directory: PathBuf::from(sandbox.working_directory()),
            host_repo_path: run_options
                .host_repo_path
                .as_ref()
                .map(|path| path.display().to_string()),
            base_branch: run_options.base_branch.clone(),
            labels: run_options.labels.clone(),
        })
        .await
        .expect("failed to seed run record in run store");
    let emitter = bound_emitter(run_options.run_id, &emitter);
    Initialized {
        graph: graph.clone(),
        source: String::new(),
        run_options: run_options.clone(),
        run_store,
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
    let initialized = initialized(
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
    )
    .await;
    let executed = pipeline::execute(initialized).await;
    persist_run_artifacts_for_tests(executed.run_store.as_ref(), &run_options.run_dir).await;
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
    let initialized = initialized(
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
    )
    .await;
    let executed = pipeline::execute(initialized).await;
    persist_run_artifacts_for_tests(executed.run_store.as_ref(), &run_options.run_dir).await;
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
    let initialized = initialized(
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
    )
    .await;
    let executed = pipeline::execute(initialized).await;
    persist_run_artifacts_for_tests(executed.run_store.as_ref(), &run_options.run_dir).await;
    executed.outcome
}

async fn persist_run_artifacts_for_tests(run_store: &dyn RunStore, run_dir: &std::path::Path) {
    if let Ok(Some(checkpoint)) = run_store.get_checkpoint().await {
        let _ = checkpoint.save(&run_dir.join("checkpoint.json"));
    }

    if let Ok(Some(final_patch)) = run_store.get_final_patch().await {
        let _ = std::fs::write(run_dir.join("final.patch"), final_patch);
    }

    for (relative_path, contents) in scan_node_files_from_store(run_store).await {
        let path = run_dir.join(relative_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, contents);
    }
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
