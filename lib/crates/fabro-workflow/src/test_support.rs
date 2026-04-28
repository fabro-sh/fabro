use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fabro_agent::Sandbox;
use fabro_auth::{CredentialSource, EnvCredentialSource};
use fabro_graphviz::graph::Graph as GvGraph;
use fabro_store::{ArtifactStore, Database, RunProjection};
use object_store::local::LocalFileSystem;

use crate::artifact_upload::ArtifactSink;
use crate::error::{Error, Result};
use crate::event::{Emitter, Event, StoreProgressLogger, append_event};
use crate::handler::HandlerRegistry;
use crate::outcome::Outcome;
use crate::pipeline;
use crate::pipeline::types::{Executed, Initialized};
use crate::pipeline::{billing_from_checkpoint, build_terminal_event};
use crate::records::Checkpoint;
use crate::run_options::RunOptions;
use crate::sandbox_metadata::SandboxGitRuntime;
use crate::services::{EngineServices, RunServices};

/// These helpers stop at EXECUTE, so they emit the terminal event here to
/// keep test consumers seeing the same end-of-run signal as production
/// (FINALIZE).
///
/// The first flush is needed because `StoreProgressLogger` forwards events
/// through an mpsc channel — without it, billing would read from a stale
/// checkpoint. The second flush ensures the just-emitted terminal event is
/// persisted before tests reopen the run store.
async fn execute_and_emit_terminal(initialized: InitializedState) -> Executed {
    let executed = Box::pin(pipeline::execute(initialized.initialized)).await;
    initialized.store_logger.flush().await;
    let state = executed.engine.run.run_store.state().await.ok();
    let billing = state
        .as_ref()
        .and_then(|s| s.checkpoint.as_ref())
        .and_then(billing_from_checkpoint);
    let event = build_terminal_event(
        &executed.outcome,
        executed.duration_ms,
        0,
        None,
        None,
        billing,
    );
    executed.engine.run.emitter.emit(&event);
    initialized.store_logger.flush().await;
    executed
}

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
    env:         HashMap<String, String>,
    checkpoint:  Option<Checkpoint>,
    llm_source:  Option<Arc<dyn CredentialSource>>,
}

struct InitializedState {
    initialized:  Initialized,
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
        None,
    ));
    let inner_store = store
        .create_run(&run_options.run_id)
        .await
        .expect("failed to create slate-backed test run store");
    let run_store = inner_store;
    append_event(&run_store, &run_options.run_id, &Event::RunCreated {
        run_id:           run_options.run_id,
        settings:         serde_json::to_value(&run_options.settings)
            .expect("failed to serialize settings"),
        graph:            serde_json::to_value(graph).expect("failed to serialize graph"),
        workflow_source:  None,
        workflow_config:  None,
        labels:           run_options
            .labels
            .clone()
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        run_dir:          run_options.run_dir.display().to_string(),
        source_directory: Some(sandbox.working_directory().to_string()),
        workflow_slug:    run_options.workflow_slug.clone(),
        db_prefix:        None,
        provenance:       None,
        manifest_blob:    None,
        git:              run_options.pre_run_git.clone(),
        fork_source_ref:  run_options.fork_source_ref.clone(),
        in_place:         false,
    })
    .await
    .expect("failed to seed run.created event in run store");
    let emitter = bound_emitter(run_options.run_id, &emitter);
    let store_logger = StoreProgressLogger::new(run_store.clone());
    store_logger.register(emitter.as_ref());
    let artifact_store = ArtifactStore::new(
        Arc::new(
            LocalFileSystem::new_with_prefix(&store_dir)
                .expect("failed to create local test artifact store"),
        ),
        "artifacts",
    );
    InitializedState {
        initialized: Initialized {
            graph:         graph.clone(),
            source:        String::new(),
            run_options:   run_options.clone(),
            checkpoint:    options.checkpoint,
            seed_context:  None,
            on_node:       None,
            artifact_sink: Some(ArtifactSink::Store(artifact_store)),
            run_control:   None,
            engine:        Arc::new(EngineServices {
                run:             RunServices::new(
                    run_store.into(),
                    emitter,
                    sandbox,
                    options.hook_runner,
                    run_options.cancel_token.clone(),
                    fabro_llm::Provider::Anthropic,
                    options
                        .llm_source
                        .unwrap_or_else(|| Arc::new(EnvCredentialSource::new())),
                    Arc::new(SandboxGitRuntime::new()),
                ),
                registry:        Arc::new(registry),
                git_state:       std::sync::RwLock::new(None),
                env:             options.env,
                inputs:          run_options.settings.run.inputs.clone(),
                dry_run:         run_options.dry_run_enabled(),
                workflow_path:   None,
                workflow_bundle: None,
            }),
            model:         String::new(),
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
            env:         HashMap::new(),
            checkpoint:  None,
            llm_source:  None,
        },
    )
    .await;
    let executed = execute_and_emit_terminal(initialized).await;
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
            env:         HashMap::new(),
            checkpoint:  None,
            llm_source:  None,
        },
    )
    .await;
    let executed = execute_and_emit_terminal(initialized).await;
    let outcome = executed.outcome?;
    let state = executed
        .engine
        .run
        .run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
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
            env:         env.unwrap_or_default(),
            checkpoint:  None,
            llm_source:  None,
        },
    )
    .await;
    let executed = execute_and_emit_terminal(initialized).await;
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
            env:         env.unwrap_or_default(),
            checkpoint:  None,
            llm_source:  None,
        },
    )
    .await;
    let executed = execute_and_emit_terminal(initialized).await;
    let outcome = executed.outcome?;
    let state = executed
        .engine
        .run
        .run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
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
            env:         HashMap::new(),
            checkpoint:  Some(checkpoint.clone()),
            llm_source:  None,
        },
    )
    .await;
    let executed = execute_and_emit_terminal(initialized).await;
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
            env:         HashMap::new(),
            checkpoint:  Some(checkpoint.clone()),
            llm_source:  None,
        },
    )
    .await;
    let executed = execute_and_emit_terminal(initialized).await;
    let outcome = executed.outcome?;
    let state = executed
        .engine
        .run
        .run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    Ok((outcome, state))
}

pub async fn run_graph_with_state_and_llm_source(
    registry: HandlerRegistry,
    emitter: Arc<Emitter>,
    sandbox: Arc<dyn Sandbox>,
    graph: &GvGraph,
    run_options: &RunOptions,
    llm_source: Arc<dyn CredentialSource>,
) -> Result<(Outcome, RunProjection)> {
    let initialized = initialized(
        registry,
        emitter,
        sandbox,
        graph,
        run_options,
        InitializedOptions {
            hook_runner: None,
            env:         HashMap::new(),
            checkpoint:  None,
            llm_source:  Some(llm_source),
        },
    )
    .await;
    let executed = pipeline::execute(initialized.initialized).await;
    initialized.store_logger.flush().await;
    let outcome = executed.outcome?;
    let state = executed
        .engine
        .run
        .run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    Ok((outcome, state))
}

pub struct WorkflowRunner {
    registry: std::sync::Mutex<Option<HandlerRegistry>>,
    emitter:  Arc<Emitter>,
    sandbox:  Arc<dyn Sandbox>,
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
        Box::pin(run_graph(
            registry,
            Arc::clone(&self.emitter),
            Arc::clone(&self.sandbox),
            graph,
            run_options,
        ))
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

    pub async fn run_with_state_and_llm_source(
        &self,
        graph: &GvGraph,
        run_options: &RunOptions,
        llm_source: Arc<dyn CredentialSource>,
    ) -> Result<(Outcome, RunProjection)> {
        let registry = self
            .registry
            .lock()
            .unwrap()
            .take()
            .expect("WorkflowRunner may only be used once");
        Box::pin(run_graph_with_state_and_llm_source(
            registry,
            Arc::clone(&self.emitter),
            Arc::clone(&self.sandbox),
            graph,
            run_options,
            llm_source,
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
        Box::pin(run_graph_from_checkpoint(
            registry,
            Arc::clone(&self.emitter),
            Arc::clone(&self.sandbox),
            graph,
            run_options,
            checkpoint,
        ))
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
