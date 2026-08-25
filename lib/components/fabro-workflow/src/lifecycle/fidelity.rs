use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_core::error::{Error as CoreError, Result as CoreResult};
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{EdgeContext, EdgeDecision, NodeDecision, RunLifecycle};
use fabro_core::state::ExecutionState;
use fabro_graphviz::graph::types::{Edge as GvEdge, Graph as GvGraph, Node as GvNode};

use crate::artifact;
use crate::context::{Context, ParallelBranchPreamble, keys};
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::handler::llm::preamble;
use crate::outcome::{BilledModelUsage, Outcome};
use crate::runtime_store::RunStoreHandle;

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeDecision = NodeDecision<Option<BilledModelUsage>>;

/// Graphviz edge captured from edge selection, passed to the next node's
/// before_node for fidelity/thread resolution.
#[derive(Debug, Clone)]
struct IncomingEdgeData {
    edge: Arc<GvEdge>,
}

/// Sub-lifecycle responsible for fidelity/thread resolution and context key
/// setup.
pub(crate) struct FidelityLifecycle {
    pub graph:                  Arc<GvGraph>,
    pub sandbox:                Arc<dyn Sandbox>,
    pub run_store:              RunStoreHandle,
    pub run_dir:                PathBuf,
    incoming_edge_data:         Mutex<Option<IncomingEdgeData>>,
    /// True on the first node after checkpoint resume when prior fidelity was
    /// Full.
    degrade_fidelity_on_resume: Mutex<bool>,
}

impl FidelityLifecycle {
    pub(crate) fn new(
        graph: Arc<GvGraph>,
        sandbox: Arc<dyn Sandbox>,
        run_store: RunStoreHandle,
        run_dir: PathBuf,
    ) -> Self {
        Self {
            graph,
            sandbox,
            run_store,
            run_dir,
            incoming_edge_data: Mutex::new(None),
            degrade_fidelity_on_resume: Mutex::new(false),
        }
    }

    pub(crate) fn set_degrade_fidelity_on_resume(&self, flag: bool) {
        *self.degrade_fidelity_on_resume.lock().expect(
            "fidelity mutex should not be poisoned: no code panics while holding this lock",
        ) = flag;
    }

    /// Render the per-branch preamble stash for a parallel node, indexed by
    /// outgoing-edge order (the same order `ParallelHandler` fans out in).
    /// `Null` entries inherit the fork's preamble.
    fn build_parallel_branch_preambles(
        &self,
        node_id: &str,
        fork_fidelity: keys::Fidelity,
        resolved_context: &Context,
        resolved_outcomes: &HashMap<String, Outcome>,
        completed_nodes: &[String],
    ) -> Vec<serde_json::Value> {
        let edges = self.graph.outgoing_edges(node_id);
        let mut preambles: Vec<serde_json::Value> = Vec::with_capacity(edges.len());
        let mut rendered: HashMap<keys::Fidelity, usize> = HashMap::new();

        for (branch_index, edge) in edges.into_iter().enumerate() {
            let Some(target_node) = self.graph.nodes.get(&edge.to) else {
                preambles.push(serde_json::Value::Null);
                continue;
            };
            let resolution = resolve_parallel_branch_fidelity(edge, target_node, fork_fidelity);
            if resolution.requested == Some(keys::Fidelity::Full) {
                tracing::warn!(
                    parallel_node = %node_id,
                    branch = %edge.to,
                    branch_index,
                    effective_fidelity = %keys::Fidelity::Full.degraded(),
                    "Parallel branch fidelity degraded from full"
                );
            }
            let Some(branch_fidelity) = resolution.effective else {
                preambles.push(serde_json::Value::Null);
                continue;
            };
            if let Some(&rendered_index) = rendered.get(&branch_fidelity) {
                preambles.push(preambles[rendered_index].clone());
                continue;
            }

            let entry = ParallelBranchPreamble {
                fidelity: branch_fidelity,
                preamble: preamble::build_preamble(
                    branch_fidelity,
                    resolved_context,
                    &self.graph,
                    completed_nodes,
                    resolved_outcomes,
                ),
            };
            rendered.insert(branch_fidelity, preambles.len());
            preambles.push(
                serde_json::to_value(entry)
                    .expect("ParallelBranchPreamble serialization cannot fail"),
            );
        }

        preambles
    }

    /// Distinct effective fidelities of a parallel node's branch preamble
    /// entries, in outgoing-edge order. Branches that inherit the fork's
    /// preamble contribute nothing beyond the fork's own fidelity.
    fn parallel_branch_effective_fidelities(
        &self,
        node_id: &str,
        fork_fidelity: keys::Fidelity,
    ) -> Vec<keys::Fidelity> {
        let mut fidelities = Vec::new();
        for edge in self.graph.outgoing_edges(node_id) {
            let Some(target_node) = self.graph.nodes.get(&edge.to) else {
                continue;
            };
            let resolution = resolve_parallel_branch_fidelity(edge, target_node, fork_fidelity);
            if let Some(fidelity) = resolution.effective {
                if !fidelities.contains(&fidelity) {
                    fidelities.push(fidelity);
                }
            }
        }
        fidelities
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for FidelityLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        // Clear incoming edge data (restart target must not inherit pre-restart edge)
        *self.incoming_edge_data.lock().expect(
            "fidelity mutex should not be poisoned: no code panics while holding this lock",
        ) = None;
        Ok(())
    }

    async fn before_node(
        &self,
        node: &WorkflowNode,
        state: &WfRunState,
    ) -> CoreResult<WfNodeDecision> {
        state.context.set(
            keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES,
            serde_json::Value::Null,
        );

        let incoming = self
            .incoming_edge_data
            .lock()
            .expect("fidelity mutex should not be poisoned: no code panics while holding this lock")
            .take();
        let gv_node = node.inner();

        // 1. Fidelity resolution via resolve_fidelity: edge → node → graph default →
        //    Compact
        let incoming_edge_ref = incoming.as_ref().map(|d| d.edge.as_ref());
        let fidelity = resolve_fidelity(incoming_edge_ref, gv_node, &self.graph);

        // 2. Fidelity degradation on resume (full → summary:high)
        let fidelity = {
            let mut degrade = self.degrade_fidelity_on_resume.lock().expect(
                "fidelity mutex should not be poisoned: no code panics while holding this lock",
            );
            if *degrade {
                *degrade = false;
                fidelity.degraded()
            } else {
                fidelity
            }
        };

        // 3. Set INTERNAL_FIDELITY
        state.context.set(
            keys::INTERNAL_FIDELITY,
            serde_json::json!(fidelity.to_string()),
        );

        // 4. Preamble building: if Full, empty preamble; otherwise build from context
        let mut resolved_values = artifact::resolved_context_snapshot(
            &state.context,
            &self.run_store,
            &*self.sandbox,
            &self.run_dir,
        )
        .await
        .map_err(|err| CoreError::Other(err.to_string()))?;
        let mut resolved_outcomes = artifact::resolve_outcomes_for_execution(
            &state.node_outcomes,
            &self.run_store,
            &*self.sandbox,
            &self.run_dir,
        )
        .await
        .map_err(|err| CoreError::Other(err.to_string()))?;

        // The resolved copies exist only to render prompt preambles, so bound
        // what any one value may contribute before the builders see them.
        // Demote exactly the values the selected fidelity renders — plus, for
        // a parallel node, whatever its branch stash renders at other
        // fidelities — so no blob or sandbox file is materialized for a value
        // absent from every generated preamble.
        let mut selection = preamble::rendered_value_selection(
            fidelity,
            &resolved_values,
            &self.graph,
            &state.completed_nodes,
            &resolved_outcomes,
        );
        if gv_node.handler_type() == Some("parallel") {
            for branch_fidelity in self.parallel_branch_effective_fidelities(node.id(), fidelity) {
                selection.merge(preamble::rendered_value_selection(
                    branch_fidelity,
                    &resolved_values,
                    &self.graph,
                    &state.completed_nodes,
                    &resolved_outcomes,
                ));
            }
        }
        if !selection.is_empty() {
            artifact::demote_large_values_for_prompt(
                &mut resolved_values,
                &mut resolved_outcomes,
                &selection,
                &self.run_store,
                &*self.sandbox,
                &self.run_dir,
            )
            .await;
        }
        let resolved_context = Context::from_values(resolved_values);

        let preamble = preamble::build_preamble(
            fidelity,
            &resolved_context,
            &self.graph,
            &state.completed_nodes,
            &resolved_outcomes,
        );
        state
            .context
            .set(keys::CURRENT_PREAMBLE, serde_json::json!(preamble));

        // 5. Parallel nodes: pre-render per-branch preambles into the stash that
        //    ParallelHandler consumes at fan-out.
        if gv_node.handler_type() == Some("parallel") {
            let branch_preambles = self.build_parallel_branch_preambles(
                node.id(),
                fidelity,
                &resolved_context,
                &resolved_outcomes,
                &state.completed_nodes,
            );
            state.context.set(
                keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES,
                serde_json::Value::Array(branch_preambles),
            );
        }

        // 6. Thread ID resolution via resolve_thread_id: edge → node → graph default →
        //    class → previous
        let thread_id = resolve_thread_id(
            incoming_edge_ref,
            gv_node,
            &self.graph,
            state.previous_node_id.as_deref(),
        );

        // 7. Set thread.{tid}.current_node
        if let Some(ref tid) = thread_id {
            let key = keys::thread_current_node_key(tid);
            state.context.set(key, serde_json::json!(node.id()));
        }

        // 8. Set INTERNAL_THREAD_ID (or null)
        match thread_id {
            Some(tid) => {
                state
                    .context
                    .set(keys::INTERNAL_THREAD_ID, serde_json::json!(tid));
            }
            None => {
                state
                    .context
                    .set(keys::INTERNAL_THREAD_ID, serde_json::Value::Null);
            }
        }

        // 9. Set INTERNAL_NODE_VISIT_COUNT and CURRENT_NODE
        let visits = state.node_visits.get(node.id()).copied().unwrap_or(1);
        state
            .context
            .set(keys::CURRENT_NODE, serde_json::json!(node.id()));
        state
            .context
            .set(keys::INTERNAL_NODE_VISIT_COUNT, serde_json::json!(visits));

        Ok(NodeDecision::Continue)
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> CoreResult<EdgeDecision> {
        // Capture fidelity/thread from edge for next node
        if let Some(ref edge) = ctx.edge {
            let gv_edge = edge.inner();
            let edge_data = IncomingEdgeData {
                edge: Arc::new(gv_edge.clone()),
            };
            *self.incoming_edge_data.lock().expect(
                "fidelity mutex should not be poisoned: no code panics while holding this lock",
            ) = Some(edge_data);
        }
        Ok(EdgeDecision::Continue)
    }
}

#[derive(Debug, Clone, Copy)]
struct ParallelBranchFidelityResolution {
    /// The explicit fidelity requested on the edge or node, pre-degradation.
    requested: Option<keys::Fidelity>,
    /// The fidelity to render an entry for; `None` inherits the fork preamble.
    effective: Option<keys::Fidelity>,
}

/// Resolve explicit branch fidelity with edge-over-node precedence.
///
/// Branches with no explicit fidelity inherit the parallel node's preamble.
/// Explicit full fidelity is degraded because concurrent branches cannot share
/// an LLM session. An effective fidelity equal to the parallel node also
/// inherits, avoiding a redundant preamble render.
fn resolve_parallel_branch_fidelity(
    edge: &GvEdge,
    target_node: &GvNode,
    parallel_fidelity: keys::Fidelity,
) -> ParallelBranchFidelityResolution {
    let requested = explicit_fidelity(Some(edge), target_node).map(|(fidelity, _)| fidelity);
    let effective = requested
        .map(keys::Fidelity::degraded)
        .filter(|fidelity| *fidelity != parallel_fidelity);

    ParallelBranchFidelityResolution {
        requested,
        effective,
    }
}

/// Explicit fidelity from the incoming edge attribute, else the node
/// attribute, with the winning source labeled for logging.
fn explicit_fidelity(
    incoming_edge: Option<&GvEdge>,
    node: &GvNode,
) -> Option<(keys::Fidelity, &'static str)> {
    incoming_edge
        .and_then(|e| e.fidelity())
        .and_then(|s| s.parse().ok())
        .map(|f| (f, "edge"))
        .or_else(|| {
            node.fidelity()
                .and_then(|s| s.parse().ok())
                .map(|f| (f, "node"))
        })
}

/// Resolve the context fidelity for a node, following the precedence:
/// 1. Incoming edge `fidelity` attribute
/// 2. Target node `fidelity` attribute
/// 3. Graph `default_fidelity` attribute
/// 4. Default: Compact
fn resolve_fidelity(
    incoming_edge: Option<&GvEdge>,
    node: &GvNode,
    graph: &GvGraph,
) -> keys::Fidelity {
    let (resolved, source) = if let Some((f, source)) = explicit_fidelity(incoming_edge, node) {
        (f, source)
    } else if let Some(f) = graph.default_fidelity().and_then(|s| s.parse().ok()) {
        (f, "graph")
    } else {
        (keys::Fidelity::default(), "default")
    };

    tracing::info!(
        node = %node.id,
        fidelity = %resolved,
        source = source,
        "Fidelity resolved"
    );

    resolved
}

/// Resolve the thread ID for a node, following the precedence:
/// 1. Incoming edge `thread_id` attribute
/// 2. Target node `thread_id` attribute
/// 3. Graph-level default thread
/// 4. Derived class from enclosing subgraph (first class from the node's
///    classes list)
/// 5. Fallback to previous node ID
fn resolve_thread_id(
    incoming_edge: Option<&GvEdge>,
    node: &GvNode,
    graph: &GvGraph,
    previous_node_id: Option<&str>,
) -> Option<String> {
    if let Some(edge) = incoming_edge {
        if let Some(tid) = edge.thread_id() {
            return Some(tid.to_string());
        }
    }
    if let Some(tid) = node.thread_id() {
        return Some(tid.to_string());
    }
    if let Some(tid) = graph.default_thread() {
        return Some(tid.to_string());
    }
    if let Some(first_class) = node.classes.first() {
        return Some(first_class.clone());
    }
    previous_node_id.map(String::from)
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests inspect materialized blob files on disk"
)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use fabro_core::graph::Graph as CoreGraph;
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_types::fixtures;
    use object_store::memory::InMemory;

    use super::*;
    use crate::context::WorkflowContext;
    use crate::context::keys::Fidelity;

    fn str_attr(value: &str) -> AttrValue {
        AttrValue::String(value.to_string())
    }

    fn parallel_workflow_graph(
        fork_fidelity: Option<&str>,
        branch_a_fidelity: Option<&str>,
    ) -> WorkflowGraph {
        let mut graph = Graph::new("parallel-fidelity");
        let mut start = Node::new("start");
        start
            .attrs
            .insert("shape".to_string(), str_attr("Mdiamond"));
        let mut fork = Node::new("fork");
        fork.attrs
            .insert("shape".to_string(), str_attr("component"));
        if let Some(fidelity) = fork_fidelity {
            fork.attrs
                .insert("fidelity".to_string(), str_attr(fidelity));
        }
        let mut branch_a = Node::new("branch_a");
        if let Some(fidelity) = branch_a_fidelity {
            branch_a
                .attrs
                .insert("fidelity".to_string(), str_attr(fidelity));
        }
        let branch_b = Node::new("branch_b");
        let mut work = Node::new("work");
        work.attrs.insert("shape".to_string(), str_attr("box"));

        graph.nodes.insert(start.id.clone(), start);
        graph.nodes.insert(fork.id.clone(), fork);
        graph.nodes.insert(branch_a.id.clone(), branch_a);
        graph.nodes.insert(branch_b.id.clone(), branch_b);
        graph.nodes.insert(work.id.clone(), work);
        graph.edges.push(Edge::new("start", "fork"));
        graph.edges.push(Edge::new("fork", "branch_a"));
        graph.edges.push(Edge::new("fork", "branch_b"));

        WorkflowGraph(Arc::new(graph))
    }

    async fn test_lifecycle(graph: &WorkflowGraph, run_dir: &Path) -> FidelityLifecycle {
        let store = Arc::new(fabro_store::test_support::test_database(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ));
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(fabro_agent::LocalSandbox::new(run_dir.to_path_buf()));
        FidelityLifecycle::new(
            graph.0.clone(),
            sandbox,
            RunStoreHandle::local(run_store),
            run_dir.to_path_buf(),
        )
    }

    /// Over the 8 KiB prompt inline budget, under the 100 KiB durable
    /// offload threshold, so values stay inline in context until demotion.
    const OVERSIZED_LEN: usize = 20_000;

    fn oversized_response() -> String {
        "R".repeat(OVERSIZED_LEN)
    }

    fn oversized_output() -> serde_json::Value {
        serde_json::json!({"rows": "O".repeat(OVERSIZED_LEN)})
    }

    fn linear_workflow_graph(work_fidelity: Option<&str>) -> WorkflowGraph {
        let mut graph = Graph::new("linear-fidelity");
        let mut start = Node::new("start");
        start
            .attrs
            .insert("shape".to_string(), str_attr("Mdiamond"));
        let mut consolidate = Node::new("consolidate");
        consolidate
            .attrs
            .insert("shape".to_string(), str_attr("box"));
        let mut work = Node::new("work");
        work.attrs.insert("shape".to_string(), str_attr("box"));
        if let Some(fidelity) = work_fidelity {
            work.attrs
                .insert("fidelity".to_string(), str_attr(fidelity));
        }

        graph.nodes.insert(start.id.clone(), start);
        graph.nodes.insert(consolidate.id.clone(), consolidate);
        graph.nodes.insert(work.id.clone(), work);
        graph.edges.push(Edge::new("start", "consolidate"));
        graph.edges.push(Edge::new("consolidate", "work"));

        WorkflowGraph(Arc::new(graph))
    }

    /// A state where the agent stage `node_id` completed with an oversized
    /// raw response and an oversized structured output, applied to context
    /// the way `ExecutionState::record` does.
    fn state_with_completed_llm_stage(graph: &WorkflowGraph, node_id: &str) -> WfRunState {
        let mut state: WfRunState = ExecutionState::new(graph).unwrap();
        let mut outcome = Outcome::success();
        outcome.context_updates.insert(
            keys::response_key(node_id),
            serde_json::json!(oversized_response()),
        );
        outcome
            .context_updates
            .insert(format!("output.{node_id}"), oversized_output());
        state.context.apply_updates(&outcome.context_updates);
        state.completed_nodes.push(node_id.to_string());
        state.node_outcomes.insert(node_id.to_string(), outcome);
        state
    }

    fn materialized_blob_files(run_dir: &Path) -> Vec<PathBuf> {
        let blobs_dir = fabro_config::RunScratch::new(run_dir)
            .runtime_dir()
            .join("blobs");
        if !blobs_dir.exists() {
            return Vec::new();
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&blobs_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        files
    }

    fn assert_durable_state_unchanged(state: &WfRunState, node_id: &str) {
        assert_eq!(
            state.context.get(&keys::response_key(node_id)),
            Some(serde_json::json!(oversized_response())),
            "durable context must keep the raw response"
        );
        assert_eq!(
            state.context.get(&format!("output.{node_id}")),
            Some(oversized_output()),
            "durable context must keep the structured output"
        );
        let outcome = &state.node_outcomes[node_id];
        assert_eq!(
            outcome.context_updates[&keys::response_key(node_id)],
            serde_json::json!(oversized_response()),
            "node outcome must keep the raw response"
        );
        assert_eq!(
            outcome.context_updates[&format!("output.{node_id}")],
            oversized_output(),
            "node outcome must keep the structured output"
        );
    }

    #[tokio::test]
    async fn compact_materializes_only_values_its_preamble_renders() {
        let graph = linear_workflow_graph(None);
        let run_dir = tempfile::tempdir().unwrap();
        let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
        let state = state_with_completed_llm_stage(&graph, "consolidate");
        let work = graph.get_node("work").unwrap();

        lifecycle.before_node(&work, &state).await.unwrap();

        let files = materialized_blob_files(run_dir.path());
        assert_eq!(
            files.len(),
            1,
            "compact must not materialize the unrendered LLM response"
        );
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        assert_eq!(stored, oversized_output());

        let preamble = state.context.preamble();
        assert!(
            preamble.contains(files[0].to_str().unwrap()),
            "the materialized output must be referenced by the preamble"
        );
        assert!(
            !preamble.contains("RRRR"),
            "the raw response must not appear in a compact preamble"
        );
        assert!(
            !preamble.contains(&"O".repeat(1000)),
            "no oversized value may be inlined"
        );
        assert_durable_state_unchanged(&state, "consolidate");
    }

    #[tokio::test]
    async fn summary_high_materializes_and_references_the_llm_response() {
        let graph = linear_workflow_graph(Some("summary:high"));
        let run_dir = tempfile::tempdir().unwrap();
        let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
        let state = state_with_completed_llm_stage(&graph, "consolidate");
        let work = graph.get_node("work").unwrap();

        lifecycle.before_node(&work, &state).await.unwrap();

        let files = materialized_blob_files(run_dir.path());
        assert_eq!(
            files.len(),
            2,
            "summary:high renders both the response and the output"
        );
        let preamble = state.context.preamble();
        for file in &files {
            assert!(
                preamble.contains(file.to_str().unwrap()),
                "every materialized blob must be referenced by the preamble: {}",
                file.display()
            );
        }
        assert!(
            !preamble.contains(&"R".repeat(1000)),
            "no oversized value may be inlined"
        );
        assert!(
            !preamble.contains(&"O".repeat(1000)),
            "no oversized value may be inlined"
        );
        assert_durable_state_unchanged(&state, "consolidate");
    }

    #[tokio::test]
    async fn value_free_fidelities_materialize_nothing() {
        for fidelity in ["full", "truncate", "summary:low"] {
            let graph = linear_workflow_graph(Some(fidelity));
            let run_dir = tempfile::tempdir().unwrap();
            let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
            let state = state_with_completed_llm_stage(&graph, "consolidate");
            let work = graph.get_node("work").unwrap();

            lifecycle.before_node(&work, &state).await.unwrap();

            assert!(
                materialized_blob_files(run_dir.path()).is_empty(),
                "{fidelity} renders no values, so nothing may be materialized"
            );
            assert_durable_state_unchanged(&state, "consolidate");
        }
    }

    #[tokio::test]
    async fn parallel_branch_materializes_values_only_its_fidelity_renders() {
        let graph = parallel_workflow_graph(None, Some("summary:high"));
        let run_dir = tempfile::tempdir().unwrap();
        let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
        let state = state_with_completed_llm_stage(&graph, "work");
        let fork = graph.get_node("fork").unwrap();

        lifecycle.before_node(&fork, &state).await.unwrap();

        // The compact fork renders the output; the summary:high branch also
        // renders the response.
        let files = materialized_blob_files(run_dir.path());
        assert_eq!(files.len(), 2);

        let fork_preamble = state.context.preamble();
        assert!(
            !fork_preamble.contains("RRRR"),
            "the compact fork preamble must not include the response"
        );

        let stash = state
            .context
            .get(keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES)
            .expect("parallel stash should be set");
        let branch_preamble = stash[0]["preamble"]
            .as_str()
            .expect("branch_a should render its own preamble");

        for file in &files {
            let path = file.to_str().unwrap();
            assert!(
                fork_preamble.contains(path) || branch_preamble.contains(path),
                "every materialized blob must be referenced by some preamble: {path}"
            );
        }
        assert!(
            branch_preamble.contains(
                materialized_blob_files(run_dir.path())
                    .iter()
                    .find_map(|file| {
                        let stored: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
                        (stored == serde_json::json!(oversized_response()))
                            .then(|| file.to_str().unwrap().to_string())
                    })
                    .expect("the raw response must be materialized")
                    .as_str()
            ),
            "the summary:high branch must reference the materialized response"
        );
    }

    #[tokio::test]
    async fn truncate_fork_materializes_only_branch_rendered_values() {
        let graph = parallel_workflow_graph(Some("truncate"), Some("compact"));
        let run_dir = tempfile::tempdir().unwrap();
        let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
        let state = state_with_completed_llm_stage(&graph, "work");
        let fork = graph.get_node("fork").unwrap();

        lifecycle.before_node(&fork, &state).await.unwrap();

        // Only the compact branch renders values, and compact omits the
        // response — so exactly the structured output is materialized.
        let files = materialized_blob_files(run_dir.path());
        assert_eq!(files.len(), 1);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        assert_eq!(stored, oversized_output());

        let stash = state
            .context
            .get(keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES)
            .expect("parallel stash should be set");
        let branch_preamble = stash[0]["preamble"].as_str().unwrap();
        assert!(
            branch_preamble.contains(files[0].to_str().unwrap()),
            "the compact branch must reference the materialized output"
        );
    }

    #[test]
    fn parallel_branch_fidelity_edge_overrides_node() {
        let mut node = Node::new("branch");
        node.attrs
            .insert("fidelity".to_string(), str_attr("compact"));
        let mut edge = Edge::new("fork", "branch");
        edge.attrs
            .insert("fidelity".to_string(), str_attr("truncate"));

        let resolved = resolve_parallel_branch_fidelity(&edge, &node, Fidelity::SummaryHigh);

        assert_eq!(resolved.requested, Some(Fidelity::Truncate));
        assert_eq!(resolved.effective, Some(Fidelity::Truncate));
    }

    #[test]
    fn parallel_branch_fidelity_without_attribute_inherits() {
        let node = Node::new("branch");
        let edge = Edge::new("fork", "branch");

        let resolution = resolve_parallel_branch_fidelity(&edge, &node, Fidelity::Compact);

        assert_eq!(resolution.requested, None);
        assert_eq!(resolution.effective, None);
    }

    #[test]
    fn parallel_branch_full_fidelity_degrades_to_summary_high() {
        let mut node = Node::new("branch");
        node.attrs.insert("fidelity".to_string(), str_attr("full"));
        let edge = Edge::new("fork", "branch");

        let resolved = resolve_parallel_branch_fidelity(&edge, &node, Fidelity::Compact);

        assert_eq!(resolved.requested, Some(Fidelity::Full));
        assert_eq!(resolved.effective, Some(Fidelity::SummaryHigh));
    }

    #[test]
    fn parallel_branch_fidelity_equal_to_fork_inherits() {
        let mut node = Node::new("branch");
        node.attrs
            .insert("fidelity".to_string(), str_attr("summary:high"));
        let edge = Edge::new("fork", "branch");

        let resolution = resolve_parallel_branch_fidelity(&edge, &node, Fidelity::SummaryHigh);

        assert_eq!(resolution.requested, Some(Fidelity::SummaryHigh));
        assert_eq!(resolution.effective, None);
    }

    #[test]
    fn explicit_full_branch_equal_to_degraded_fork_inherits() {
        let mut node = Node::new("branch");
        node.attrs.insert("fidelity".to_string(), str_attr("full"));
        let edge = Edge::new("fork", "branch");

        let resolution = resolve_parallel_branch_fidelity(&edge, &node, Fidelity::SummaryHigh);

        assert_eq!(resolution.requested, Some(Fidelity::Full));
        assert_eq!(resolution.effective, None);
    }

    #[test]
    fn full_fork_without_branch_fidelity_does_not_create_entry() {
        let node = Node::new("branch");
        let edge = Edge::new("fork", "branch");

        let resolution = resolve_parallel_branch_fidelity(&edge, &node, Fidelity::Full);

        assert_eq!(resolution.requested, None);
        assert_eq!(resolution.effective, None);
    }

    #[tokio::test]
    async fn parallel_before_node_rebuilds_branch_preamble_stash() {
        let graph = parallel_workflow_graph(None, Some("truncate"));
        let run_dir = tempfile::tempdir().unwrap();
        let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
        let state: WfRunState = ExecutionState::new(&graph).unwrap();
        let fork = graph.get_node("fork").unwrap();

        lifecycle.before_node(&fork, &state).await.unwrap();
        state.context.set(
            keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES,
            serde_json::json!(["stale", "entries", "must disappear"]),
        );
        lifecycle.before_node(&fork, &state).await.unwrap();

        let stash = state
            .context
            .get(keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES)
            .expect("parallel stash should be set");
        let entries = stash.as_array().expect("parallel stash should be an array");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_object());
        assert!(entries[1].is_null());
    }

    #[tokio::test]
    async fn non_parallel_before_node_overwrites_branch_preamble_stash_with_null() {
        let graph = parallel_workflow_graph(None, Some("truncate"));
        let run_dir = tempfile::tempdir().unwrap();
        let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
        let state: WfRunState = ExecutionState::new(&graph).unwrap();
        let fork = graph.get_node("fork").unwrap();
        let work = graph.get_node("work").unwrap();

        lifecycle.before_node(&fork, &state).await.unwrap();
        lifecycle.before_node(&work, &state).await.unwrap();

        assert_eq!(
            state.context.get(keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES),
            Some(serde_json::Value::Null)
        );
    }

    #[tokio::test]
    async fn resumed_full_fork_degrades_without_rendering_fallback_branches() {
        let graph = parallel_workflow_graph(Some("full"), None);
        let run_dir = tempfile::tempdir().unwrap();
        let lifecycle = test_lifecycle(&graph, run_dir.path()).await;
        lifecycle.set_degrade_fidelity_on_resume(true);
        let state: WfRunState = ExecutionState::new(&graph).unwrap();
        let fork = graph.get_node("fork").unwrap();

        lifecycle.before_node(&fork, &state).await.unwrap();

        assert_eq!(state.context.fidelity(), Fidelity::SummaryHigh);
        assert_eq!(
            state.context.get(keys::INTERNAL_PARALLEL_BRANCH_PREAMBLES),
            Some(serde_json::json!([null, null]))
        );
    }

    #[test]
    fn fidelity_defaults_to_compact() {
        let node = Node::new("work");
        let graph = Graph::new("test");
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Compact);
    }

    #[test]
    fn fidelity_from_graph_default() {
        let node = Node::new("work");
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Truncate);
    }

    #[test]
    fn fidelity_from_node_overrides_graph() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Full);
    }

    #[test]
    fn fidelity_from_edge_overrides_node() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        let mut edge = Edge::new("a", "work");
        edge.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("summary:high".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_fidelity(Some(&edge), &node, &graph),
            Fidelity::SummaryHigh
        );
    }

    #[test]
    fn thread_id_from_node_attribute() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("main-thread".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("main-thread".to_string())
        );
    }

    #[test]
    fn thread_id_from_edge_attribute() {
        let node = Node::new("work");
        let mut edge = Edge::new("prev", "work");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("edge-thread".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("edge-thread".to_string())
        );
    }

    #[test]
    fn thread_id_node_used_when_no_edge_thread() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("node-thread".to_string()),
        );
        let edge = Edge::new("prev", "work");
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("node-thread".to_string())
        );
    }

    #[test]
    fn thread_id_edge_overrides_node() {
        let mut node = Node::new("work");
        node.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("node-thread".to_string()),
        );
        let mut edge = Edge::new("prev", "work");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("edge-thread".to_string()),
        );
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("edge-thread".to_string()),
            "edge thread_id should override node thread_id"
        );
    }

    #[test]
    fn thread_id_from_graph_default_thread() {
        let node = Node::new("work");
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("shared-thread".to_string()),
        );
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("shared-thread".to_string())
        );
    }

    #[test]
    fn thread_id_edge_overrides_graph_default() {
        let node = Node::new("work");
        let mut edge = Edge::new("prev", "work");
        edge.attrs.insert(
            "thread_id".to_string(),
            AttrValue::String("edge-thread".to_string()),
        );
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("shared-thread".to_string()),
        );
        assert_eq!(
            resolve_thread_id(Some(&edge), &node, &graph, Some("prev")),
            Some("edge-thread".to_string())
        );
    }

    #[test]
    fn thread_id_graph_default_overrides_class() {
        let mut node = Node::new("work");
        node.classes = vec!["planning".to_string()];
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_thread".to_string(),
            AttrValue::String("shared-thread".to_string()),
        );
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("shared-thread".to_string())
        );
    }

    #[test]
    fn thread_id_from_node_class() {
        let mut node = Node::new("work");
        node.classes = vec!["planning".to_string(), "review".to_string()];
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev")),
            Some("planning".to_string())
        );
    }

    #[test]
    fn thread_id_fallback_to_previous_node() {
        let node = Node::new("work");
        let graph = Graph::new("test");
        assert_eq!(
            resolve_thread_id(None, &node, &graph, Some("prev_node")),
            Some("prev_node".to_string())
        );
    }

    #[test]
    fn thread_id_none_when_no_sources() {
        let node = Node::new("start");
        let graph = Graph::new("test");
        assert_eq!(resolve_thread_id(None, &node, &graph, None), None);
    }
}
