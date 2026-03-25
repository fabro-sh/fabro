use std::sync::Arc;
use std::time::Instant;

use fabro_core::executor::ExecutorBuilder;
use fabro_core::state::RunState;
use tokio_util::sync::CancellationToken;

use crate::context::{self, Context};
use crate::core_adapter::{WorkflowGraph, WorkflowLifecycle, WorkflowNodeHandler};
use crate::error::FabroError;
use crate::handler::EngineServices;
use crate::outcome::{Outcome, StageStatus};
use crate::sandbox_git::GitState;

use super::types::{Executed, Initialized};

fn seed_context_from_checkpoint(checkpoint: Option<&crate::checkpoint::Checkpoint>) -> Context {
    let context = Context::new();
    if let Some(cp) = checkpoint {
        for (k, v) in &cp.context_values {
            context.set(k.clone(), v.clone());
        }
    }
    context
}

/// EXECUTE phase: run the workflow graph.
///
/// Infallible at the function level — engine errors are captured in `outcome`.
pub async fn execute(init: Initialized) -> Executed {
    let Initialized {
        graph,
        source: _,
        settings,
        checkpoint,
        seed_context,
        emitter,
        sandbox,
        registry,
        hook_runner,
        env,
        dry_run,
    } = init;

    let start = Instant::now();
    let graph_arc = Arc::new(graph.clone());
    let wf_graph = WorkflowGraph(Arc::clone(&graph_arc));

    let git_state = settings.git.as_ref().and_then(|git| {
        let base_sha = git.base_sha.clone()?;
        Some(Arc::new(GitState {
            run_id: settings.run_id.clone(),
            base_sha,
            run_branch: git.run_branch.clone(),
            meta_branch: git.meta_branch.clone(),
            checkpoint_exclude_globs: settings.checkpoint_exclude_globs().to_vec(),
            git_author: settings.git_author.clone(),
        }))
    });

    let shared_services = Arc::new(EngineServices {
        registry,
        emitter: Arc::clone(&emitter),
        sandbox: Arc::clone(&sandbox),
        git_state: std::sync::RwLock::new(git_state),
        hook_runner: hook_runner.clone(),
        env,
        dry_run,
    });

    let handler = Arc::new(WorkflowNodeHandler {
        services: shared_services,
        run_dir: settings.run_dir.clone(),
        graph: Arc::clone(&graph_arc),
    });

    let settings_arc = Arc::new(settings.clone());
    let lifecycle = WorkflowLifecycle::new(
        Arc::clone(&emitter),
        hook_runner.clone(),
        Arc::clone(&sandbox),
        graph_arc,
        settings.run_dir.clone(),
        settings_arc,
        checkpoint.is_some(),
    );

    if let Some(ref cp) = checkpoint {
        lifecycle.restore_circuit_breaker(
            cp.loop_failure_signatures.clone(),
            cp.restart_failure_signatures.clone(),
        );
        if cp.context_values.get(context::keys::INTERNAL_FIDELITY)
            == Some(&serde_json::json!(context::keys::Fidelity::Full.to_string()))
        {
            lifecycle.set_degrade_fidelity_on_resume(true);
        }
    }

    let state = if let Some(ref cp) = checkpoint {
        match RunState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string())) {
            Ok(mut s) => {
                for (k, v) in &cp.context_values {
                    s.context.set(k.clone(), v.clone());
                }
                s.completed_nodes = cp.completed_nodes.clone();
                s.node_retries = cp.node_retries.clone();
                if cp.node_visits.is_empty() {
                    for id in &cp.completed_nodes {
                        *s.node_visits.entry(id.clone()).or_insert(0) += 1;
                    }
                } else {
                    s.node_visits = cp.node_visits.clone();
                }
                for (k, v) in &cp.node_outcomes {
                    s.node_outcomes.insert(k.clone(), v.clone());
                }
                s.stage_index = cp.completed_nodes.len();
                if let Some(ref next) = cp.next_node_id {
                    s.current_node_id = next.clone();
                } else {
                    let edges = graph.outgoing_edges(&cp.current_node);
                    if let Some(edge) = edges.first() {
                        s.current_node_id = edge.to.clone();
                    } else {
                        s.current_node_id = cp.current_node.clone();
                    }
                }
                s
            }
            Err(err) => {
                return Executed {
                    graph,
                    outcome: Err(err),
                    settings,
                    hook_runner,
                    emitter,
                    sandbox,
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: seed_context_from_checkpoint(checkpoint.as_ref()),
                };
            }
        }
    } else if let Some(seed) = seed_context {
        match RunState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string())) {
            Ok(s) => {
                for (k, v) in seed.snapshot() {
                    s.context.set(k, v);
                }
                s
            }
            Err(err) => {
                return Executed {
                    graph,
                    outcome: Err(err),
                    settings,
                    hook_runner,
                    emitter,
                    sandbox,
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: seed,
                };
            }
        }
    } else {
        match RunState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string())) {
            Ok(s) => s,
            Err(err) => {
                return Executed {
                    graph,
                    outcome: Err(err),
                    settings,
                    hook_runner,
                    emitter,
                    sandbox,
                    duration_ms: crate::millis_u64(start.elapsed()),
                    final_context: Context::new(),
                };
            }
        }
    };

    let initial_context = state.context.clone();

    let graph_max = graph.max_node_visits();
    let max_node_visits = if graph_max > 0 {
        Some(graph_max as usize)
    } else if settings.dry_run {
        Some(10)
    } else {
        None
    };

    let stall_timeout_opt = graph.stall_timeout();
    let stall_token = stall_timeout_opt.map(|_| CancellationToken::new());
    let stall_shutdown =
        if let (Some(stall_timeout), Some(ref token)) = (stall_timeout_opt, &stall_token) {
            let shutdown = CancellationToken::new();
            let emitter = Arc::clone(&emitter);
            let token_clone = token.clone();
            let shutdown_clone = shutdown.clone();
            emitter.touch();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(stall_timeout) => {
                            if shutdown_clone.is_cancelled() {
                                return;
                            }
                            let last = emitter.last_event_at();
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as i64;
                            let idle_ms = now.saturating_sub(last);
                            if idle_ms >= stall_timeout.as_millis() as i64 {
                                token_clone.cancel();
                                return;
                            }
                        }
                        _ = shutdown_clone.cancelled() => {
                            return;
                        }
                    }
                }
            });
            Some(shutdown)
        } else {
            None
        };

    let mut builder =
        ExecutorBuilder::new(handler as Arc<dyn fabro_core::handler::NodeHandler<WorkflowGraph>>)
            .lifecycle(Box::new(lifecycle));

    if let Some(ref cancel) = settings.cancel_token {
        builder = builder.cancel_token(cancel.clone());
    }
    if let Some(token) = stall_token.clone() {
        builder = builder.stall_token(token);
    }
    if let Some(limit) = max_node_visits {
        builder = builder.max_node_visits(limit);
    }

    let executor = builder.build();
    let result = executor.run(&wf_graph, state).await;

    if let Some(shutdown) = stall_shutdown {
        shutdown.cancel();
    }

    let (outcome, final_context) = match result {
        Ok((core_outcome, final_state)) => {
            let ctx = final_state.context.clone();
            let result = if core_outcome.status == StageStatus::Fail {
                core_outcome
            } else {
                let mut out = Outcome::success();
                out.notes = Some("Pipeline completed".to_string());
                out
            };
            (Ok(result), ctx)
        }
        Err(fabro_core::CoreError::StallTimeout { node_id }) => {
            let stall_timeout = graph.stall_timeout().unwrap_or_default();
            let idle_secs = stall_timeout.as_secs();
            emitter.emit(&crate::event::WorkflowRunEvent::StallWatchdogTimeout {
                node: node_id.clone(),
                idle_seconds: idle_secs,
            });
            (
                Err(FabroError::engine(format!(
                    "stall watchdog: node \"{node_id}\" had no activity for {idle_secs}s"
                ))),
                initial_context,
            )
        }
        Err(fabro_core::CoreError::Cancelled) => (Err(FabroError::Cancelled), initial_context),
        Err(fabro_core::CoreError::Blocked { message }) => {
            (Err(FabroError::engine(message)), initial_context)
        }
        Err(e) => (Err(FabroError::engine(e.to_string())), initial_context),
    };

    let duration_ms = crate::millis_u64(start.elapsed());

    Executed {
        graph,
        outcome,
        settings,
        hook_runner,
        emitter,
        sandbox,
        duration_ms,
        final_context,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fabro_config::config::FabroConfig;
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_interview::AutoApproveInterviewer;

    use super::*;
    use crate::handler::default_registry;
    use crate::pipeline::initialize;
    use crate::pipeline::types::{InitOptions, Validated};
    use crate::run_settings::{LifecycleConfig, RunSettings};

    fn simple_graph() -> (Graph, String) {
        let source =
            "digraph test { start [shape=Mdiamond]; exit [shape=Msquare]; start -> exit; }"
                .to_string();
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        graph.nodes.insert("exit".to_string(), exit);
        graph.edges.push(Edge::new("start", "exit"));
        (graph, source)
    }

    fn test_settings(run_dir: &std::path::Path) -> RunSettings {
        RunSettings {
            config: FabroConfig::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "run-test".to_string(),
            labels: HashMap::new(),
            git_author: crate::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            host_repo_path: None,
            base_branch: None,
            git: None,
        }
    }

    #[tokio::test]
    async fn execute_runs_start_to_exit_and_returns_final_context() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = simple_graph();
        let initialized = initialize(
            Validated::new(graph, source, vec![]),
            InitOptions {
                run_id: "run-test".to_string(),
                run_dir: run_dir.clone(),
                dry_run: false,
                emitter: Arc::new(crate::event::EventEmitter::new()),
                sandbox: Arc::new(fabro_agent::LocalSandbox::new(
                    std::env::current_dir().unwrap(),
                )),
                registry: Arc::new(default_registry(Arc::new(AutoApproveInterviewer), || None)),
                lifecycle: LifecycleConfig {
                    setup_commands: vec![],
                    setup_command_timeout_ms: 1_000,
                    devcontainer_phases: vec![],
                },
                run_settings: test_settings(&run_dir),
                hooks: fabro_hooks::HookConfig { hooks: vec![] },
                sandbox_env: HashMap::new(),
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
}

#[cfg(test)]
#[path = "execute_engine_compat_tests.rs"]
mod engine_compat_tests;
