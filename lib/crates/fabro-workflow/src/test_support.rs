use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fabro_agent::Sandbox;
use fabro_graphviz::graph::Graph as GvGraph;
use fabro_store::{Database, RunProjection};
use object_store::local::LocalFileSystem;

use crate::error::{FabroError, Result};
use crate::event::{Emitter, Event, StoreProgressLogger, append_event};
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use crate::pipeline;
use crate::pipeline::types::Initialized;
use crate::records::Checkpoint;
use crate::run_options::RunOptions;

pub fn test_store_dir(run_dir: &std::path::Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    run_dir.hash(&mut hasher);
    std::env::temp_dir()
        .join("fabro-test-run-stores")
        .join(format!("{:016x}", hasher.finish()))
}

struct InitializedOptions {
    hook_runner: Option<Arc<fabro_hooks::HookRunner>>,
    env: HashMap<String, String>,
    checkpoint: Option<Checkpoint>,
}

struct InitializedState {
    initialized: Initialized,
    store_logger: StoreProgressLogger,
}

fn bound_emitter(run_id: fabro_types::RunId, observer: &Arc<Emitter>) -> Arc<Emitter> {
    let emitter = Arc::new(Emitter::new(run_id));
    let observer_clone = Arc::clone(observer);
    emitter.on_event(move |event| observer_clone.dispatch_run_event(event));
    emitter
}

async fn initialized(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    options: InitializedOptions,
) -> InitializedState {
    std::fs::create_dir_all(&run_options.run_dir).expect("failed to create run dir");
    let store_dir = test_store_dir(&run_options.run_dir);
    let _ = std::fs::remove_dir_all(&store_dir);
    std::fs::create_dir_all(&store_dir).expect("failed to create local test run store dir");
    let store = Arc::new(Database::new(
        Arc::new(
            LocalFileSystem::new_with_prefix(&store_dir)
                .expect("failed to create local test run store"),
        ),
        "",
        Duration::from_millis(1),
    ));
    let inner_store = store
        .create_run(&run_options.run_id)
        .await
        .expect("failed to create slate-backed test run store");
    let run_store = inner_store;
    append_event(
        &run_store,
        &run_options.run_id,
        &Event::RunCreated {
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
            repo_origin_url: None,
            base_branch: run_options.base_branch.clone(),
            workflow_slug: run_options.workflow_slug.clone(),
            db_prefix: None,
            provenance: None,
            manifest_blob: None,
        },
    )
    .await
    .expect("failed to seed run.created event in run store");
    let emitter = bound_emitter(run_options.run_id, &emitter);
    let store_logger = StoreProgressLogger::new(run_store.clone());
    store_logger.register(emitter.as_ref());
    InitializedState {
        initialized: Initialized {
            graph: graph.clone(),
            source: String::new(),
            run_options: run_options.clone(),
            workflow_path: None,
            workflow_bundle: None,
            run_store: run_store.into(),
            checkpoint: options.checkpoint,
            seed_context: None,
            emitter,
            sandbox,
            registry: Arc::new(registry),
            on_node: None,
            artifact_uploader: None,
            run_control: None,
            hook_runner: options.hook_runner,
            env: options.env,
            dry_run: run_options.dry_run_enabled(),
            llm_client: None,
            model: String::new(),
            provider: fabro_llm::Provider::Anthropic,
        },
        store_logger,
    }
}

pub async fn run_graph(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
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
    let executed = pipeline::execute(initialized.initialized).await;
    executed.outcome
}

pub async fn run_graph_with_state(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
) -> Result<(Outcome, RunProjection)> {
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
    let executed = pipeline::execute(initialized.initialized).await;
    let outcome = executed.outcome?;
    initialized.store_logger.flush().await;
    let state = executed
        .run_store
        .state()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?;
    Ok((outcome, state))
}

pub async fn run_graph_with_hooks(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
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
    let executed = pipeline::execute(initialized.initialized).await;
    executed.outcome
}

pub async fn run_graph_with_hooks_and_state(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    hook_runner: Arc<fabro_hooks::HookRunner>,
    env: Option<HashMap<String, String>>,
) -> Result<(Outcome, RunProjection)> {
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
    let executed = pipeline::execute(initialized.initialized).await;
    let outcome = executed.outcome?;
    initialized.store_logger.flush().await;
    let state = executed
        .run_store
        .state()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?;
    Ok((outcome, state))
}

pub async fn run_graph_from_checkpoint(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
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
    let executed = pipeline::execute(initialized.initialized).await;
    executed.outcome
}

pub async fn run_graph_from_checkpoint_with_state(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    checkpoint: &Checkpoint,
) -> Result<(Outcome, RunProjection)> {
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
    let executed = pipeline::execute(initialized.initialized).await;
    let outcome = executed.outcome?;
    initialized.store_logger.flush().await;
    let state = executed
        .run_store
        .state()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?;
    Ok((outcome, state))
}

pub struct WorkflowRunner {
    registry: std::sync::Mutex<Option<HandlerRegistry>>,
    emitter: Arc<Emitter>,
    sandbox: Arc<dyn Sandbox>,
}

impl WorkflowRunner {
    #[must_use]
    pub fn new(
        registry: HandlerRegistry,
        emitter: Arc<Emitter>,
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

    pub async fn run_with_state(
        &self,
        graph: &GvGraph,
        run_options: &RunOptions,
    ) -> Result<(Outcome, RunProjection)> {
        let registry = self
            .registry
            .lock()
            .unwrap()
            .take()
            .expect("WorkflowRunner may only be used once");
        Box::pin(run_graph_with_state(
            registry,
            Arc::clone(&self.emitter),
            Arc::clone(&self.sandbox),
            graph,
            run_options,
        ))
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

    pub async fn run_from_checkpoint_with_state(
        &self,
        graph: &GvGraph,
        run_options: &RunOptions,
        checkpoint: &Checkpoint,
    ) -> Result<(Outcome, RunProjection)> {
        let registry = self
            .registry
            .lock()
            .unwrap()
            .take()
            .expect("WorkflowRunner may only be used once");
        Box::pin(run_graph_from_checkpoint_with_state(
            registry,
            Arc::clone(&self.emitter),
            Arc::clone(&self.sandbox),
            graph,
            run_options,
            checkpoint,
        ))
        .await
    }
}
