use std::path::Path;
use std::time::Instant;

use async_trait::async_trait;

use crate::context::Context;
use crate::engine::select_edge;
use crate::error::ArcError;
use crate::event::PipelineEvent;
use crate::graph::{Graph, Node};
use crate::outcome::Outcome;
use crate::pipeline::prepare_pipeline;

use super::{EngineServices, Handler};

/// Convert a Duration's milliseconds to u64, saturating on overflow.
fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Executes a sub-pipeline defined by inline DOT source in a node attribute.
/// The sub-pipeline runs with a cloned context; context updates propagate back.
pub struct SubPipelineHandler;

/// Check whether a node is a terminal (exit) node.
fn is_terminal(node: &Node) -> bool {
    node.shape() == "Msquare" || node.handler_type() == Some("exit")
}

#[async_trait]
impl Handler for SubPipelineHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        logs_root: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, ArcError> {
        // 1. Get DOT source from node attribute
        let dot_source = match node
            .attrs
            .get("sub_pipeline.dot_source")
            .and_then(|v| v.as_str())
        {
            Some(s) if !s.is_empty() => s,
            _ => {
                return Ok(Outcome::fail(
                    "No sub_pipeline.dot_source attribute specified",
                ))
            }
        };

        // 2. Parse the sub-pipeline DOT
        let sub_graph = match prepare_pipeline(dot_source) {
            Ok(g) => g,
            Err(e) => return Ok(Outcome::fail(format!("Failed to parse sub-pipeline: {e}"))),
        };

        // 3. Find start node
        let start_node = match sub_graph.find_start_node() {
            Some(n) => n.id.clone(),
            None => return Ok(Outcome::fail("Sub-pipeline has no start node")),
        };

        // 4. Clone parent context for isolation
        let sub_context = context.clone_context();
        let before_snapshot = context.snapshot();

        // 5. Walk the sub-graph
        let visit = crate::engine::visit_from_context(context);
        let sub_logs_root = crate::engine::node_dir(logs_root, &node.id, visit);
        let mut current_node_id = start_node.clone();
        let mut last_outcome = Outcome::success();

        services.emitter.emit(&PipelineEvent::SubgraphStarted {
            node_id: node.id.clone(),
            start_node,
        });
        let subgraph_start = Instant::now();

        let max_steps: usize = 1000;
        let mut steps: usize = 0;

        while steps < max_steps {
            steps += 1;

            let sub_node = match sub_graph.nodes.get(&current_node_id) {
                Some(n) => n,
                None => {
                    return Ok(Outcome::fail(format!(
                        "Sub-pipeline node not found: {current_node_id}"
                    )));
                }
            };

            // Check for terminal node
            if is_terminal(sub_node) {
                break;
            }

            // Execute the node handler
            let handler = services.registry.resolve(sub_node);
            last_outcome = handler
                .execute(sub_node, &sub_context, &sub_graph, &sub_logs_root, services)
                .await?;

            // Apply context updates from the outcome
            sub_context.apply_updates(&last_outcome.context_updates);
            sub_context.set(
                "outcome",
                serde_json::json!(last_outcome.status.to_string()),
            );

            // Select next edge
            match select_edge(&current_node_id, &last_outcome, &sub_context, &sub_graph) {
                Some(edge) => {
                    current_node_id.clone_from(&edge.to);
                }
                None => break,
            }
        }

        services.emitter.emit(&PipelineEvent::SubgraphCompleted {
            node_id: node.id.clone(),
            steps_executed: steps,
            status: last_outcome.status.to_string(),
            duration_ms: millis_u64(subgraph_start.elapsed()),
        });

        // 6. Compute context diff (sub_context changes vs parent's original snapshot)
        let after_snapshot = sub_context.snapshot();
        let mut context_updates = std::collections::HashMap::new();
        for (key, value) in &after_snapshot {
            match before_snapshot.get(key) {
                Some(old_value) if old_value == value => {}
                _ => {
                    context_updates.insert(key.clone(), value.clone());
                }
            }
        }

        // 7. Return the last outcome with context updates propagated
        let mut result = last_outcome;
        result.context_updates.extend(context_updates);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventEmitter;
    use crate::graph::AttrValue;
    use crate::handler::exit::ExitHandler;
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;
    use crate::outcome::StageStatus;
    use std::sync::Arc;

    fn local_env() -> Arc<dyn arc_agent::ExecutionEnvironment> {
        Arc::new(arc_agent::LocalExecutionEnvironment::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        ))
    }

    fn make_services() -> EngineServices {
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        EngineServices {
            registry: Arc::new(registry),
            emitter: Arc::new(EventEmitter::new()),
            execution_env: local_env(),
        }
    }

    fn make_services_with_registry(registry: HandlerRegistry) -> EngineServices {
        EngineServices {
            registry: Arc::new(registry),
            emitter: Arc::new(EventEmitter::new()),
            execution_env: local_env(),
        }
    }

    #[tokio::test]
    async fn executes_simple_sub_pipeline() {
        let services = make_services();

        let mut node = Node::new("sub");
        node.attrs.insert(
            "sub_pipeline.dot_source".to_string(),
            AttrValue::String(
                r"digraph Sub {
                    start [shape=Mdiamond]
                    exit [shape=Msquare]
                    start -> exit
                }"
                .to_string(),
            ),
        );

        let context = Context::new();
        let graph = Graph::new("parent");
        let tmp = tempfile::tempdir().unwrap();

        let outcome = SubPipelineHandler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn parent_context_available_in_sub_pipeline() {
        let services = make_services();

        let mut node = Node::new("sub");
        node.attrs.insert(
            "sub_pipeline.dot_source".to_string(),
            AttrValue::String(
                r"digraph Sub {
                    start [shape=Mdiamond]
                    exit [shape=Msquare]
                    start -> exit
                }"
                .to_string(),
            ),
        );

        let context = Context::new();
        context.set("parent.value", serde_json::json!("hello"));
        let graph = Graph::new("parent");
        let tmp = tempfile::tempdir().unwrap();

        let outcome = SubPipelineHandler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        // The sub-pipeline clones the context, so the parent value should be
        // available during sub-execution. After execution, any sub-pipeline
        // context updates should be in the outcome's context_updates.
    }

    #[tokio::test]
    async fn context_updates_propagate_back() {
        // Use a handler that sets a context value, register it in the sub-pipeline registry
        struct ContextSettingHandler;

        #[async_trait]
        impl Handler for ContextSettingHandler {
            async fn execute(
                &self,
                _node: &Node,
                context: &Context,
                _graph: &Graph,
                _logs_root: &Path,
                _services: &EngineServices,
            ) -> Result<Outcome, ArcError> {
                context.set("sub.result", serde_json::json!("from_sub"));
                Ok(Outcome::success())
            }
        }

        let mut registry = HandlerRegistry::new(Box::new(ContextSettingHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        let services = make_services_with_registry(registry);

        let mut node = Node::new("sub");
        node.attrs.insert(
            "sub_pipeline.dot_source".to_string(),
            AttrValue::String(
                r"digraph Sub {
                    start [shape=Mdiamond]
                    work [shape=box]
                    exit [shape=Msquare]
                    start -> work -> exit
                }"
                .to_string(),
            ),
        );

        let context = Context::new();
        let graph = Graph::new("parent");
        let tmp = tempfile::tempdir().unwrap();

        let outcome = SubPipelineHandler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);

        // Context updates from the sub-pipeline should be in the outcome
        assert!(
            outcome.context_updates.contains_key("sub.result"),
            "sub-pipeline context updates should propagate back"
        );
        assert_eq!(
            outcome.context_updates.get("sub.result"),
            Some(&serde_json::json!("from_sub"))
        );
    }

    #[tokio::test]
    async fn failing_sub_pipeline_returns_fail() {
        struct AlwaysFailHandler;

        #[async_trait]
        impl Handler for AlwaysFailHandler {
            async fn execute(
                &self,
                _node: &Node,
                _context: &Context,
                _graph: &Graph,
                _logs_root: &Path,
                _services: &EngineServices,
            ) -> Result<Outcome, ArcError> {
                Ok(Outcome::fail("sub-pipeline failure"))
            }
        }

        let mut registry = HandlerRegistry::new(Box::new(AlwaysFailHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        let services = make_services_with_registry(registry);

        let mut node = Node::new("sub");
        // Sub-pipeline where the work node fails and there's a fail edge to exit
        node.attrs.insert(
            "sub_pipeline.dot_source".to_string(),
            AttrValue::String(
                r#"digraph Sub {
                    start [shape=Mdiamond]
                    work [shape=box, max_retries="0"]
                    exit [shape=Msquare]
                    start -> work
                    work -> exit [condition="outcome=fail"]
                }"#
                .to_string(),
            ),
        );

        let context = Context::new();
        let graph = Graph::new("parent");
        let tmp = tempfile::tempdir().unwrap();

        let outcome = SubPipelineHandler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
    }

    #[tokio::test]
    async fn missing_dot_source_returns_fail() {
        let services = make_services();

        let node = Node::new("sub");
        let context = Context::new();
        let graph = Graph::new("parent");
        let tmp = tempfile::tempdir().unwrap();

        let outcome = SubPipelineHandler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(
            outcome
                .failure_reason
                .as_deref()
                .unwrap()
                .contains("sub_pipeline.dot_source"),
            "should mention the missing attribute"
        );
    }

    #[tokio::test]
    async fn invalid_dot_source_returns_fail() {
        let services = make_services();

        let mut node = Node::new("sub");
        node.attrs.insert(
            "sub_pipeline.dot_source".to_string(),
            AttrValue::String("not valid dot".to_string()),
        );

        let context = Context::new();
        let graph = Graph::new("parent");
        let tmp = tempfile::tempdir().unwrap();

        let outcome = SubPipelineHandler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
    }
}
