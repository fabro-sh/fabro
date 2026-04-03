use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use fabro_agent::Sandbox;
use fabro_graphviz::graph::Graph as GvGraph;
use fabro_store::SlateStore;
use object_store::memory::InMemory;

use crate::error::Result;
use crate::event::{EventEmitter, WorkflowRunEvent, append_workflow_event};
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
    let store = Arc::new(SlateStore::new(
        Arc::new(InMemory::new()),
        "",
        Duration::from_millis(1),
    ));
    let inner_store = store
        .create_run(
            &run_options.run_id,
            created_at,
            Some(run_options.run_dir.to_string_lossy().as_ref()),
        )
        .await
        .expect("failed to create slate-backed test run store");
    let run_store = inner_store;
    append_workflow_event(
        &run_store,
        &run_options.run_id,
        &WorkflowRunEvent::RunCreated {
            run_id: run_options.run_id,
            settings: serde_json::to_value(&run_options.settings)
                .expect("failed to serialize settings"),
            graph: serde_json::to_value(graph).expect("failed to serialize graph"),
            workflow_source: None,
            workflow_config: None,
            labels: run_options
                .labels
                .clone()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            run_dir: run_options.run_dir.display().to_string(),
            working_directory: PathBuf::from(sandbox.working_directory())
                .display()
                .to_string(),
            host_repo_path: run_options
                .host_repo_path
                .as_ref()
                .map(|path| path.display().to_string()),
            base_branch: run_options.base_branch.clone(),
            workflow_slug: run_options.workflow_slug.clone(),
            db_prefix: None,
        },
    )
    .await
    .expect("failed to seed run.created event in run store");
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
