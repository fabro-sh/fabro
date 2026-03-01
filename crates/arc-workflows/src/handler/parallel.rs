use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::Semaphore;

use crate::context::Context;
use crate::error::ArcError;
use crate::event::PipelineEvent;
use crate::graph::{Graph, Node};
use crate::outcome::{Outcome, StageStatus};

use super::{EngineServices, Handler};

/// Convert a Duration's milliseconds to u64, saturating on overflow.
fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Fans out execution to multiple branches concurrently.
/// Each branch gets an isolated context clone and runs independently.
pub struct ParallelHandler;

/// Parse join policy from node attributes.
#[derive(Debug, Clone)]
enum JoinPolicy {
    WaitAll,
    FirstSuccess,
    KOfN(usize),
    Quorum(f64),
}

impl std::fmt::Display for JoinPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WaitAll => write!(f, "wait_all"),
            Self::FirstSuccess => write!(f, "first_success"),
            Self::KOfN(k) => write!(f, "k_of_n({k})"),
            Self::Quorum(frac) => write!(f, "quorum({frac})"),
        }
    }
}

fn parse_join_policy(raw: &str) -> JoinPolicy {
    if raw == "first_success" {
        return JoinPolicy::FirstSuccess;
    }
    if let Some(inner) = raw
        .strip_prefix("k_of_n(")
        .and_then(|s| s.strip_suffix(')'))
    {
        if let Ok(k) = inner.trim().parse::<usize>() {
            return JoinPolicy::KOfN(k);
        }
    }
    if let Some(inner) = raw
        .strip_prefix("quorum(")
        .and_then(|s| s.strip_suffix(')'))
    {
        if let Ok(frac) = inner.trim().parse::<f64>() {
            return JoinPolicy::Quorum(frac);
        }
    }
    JoinPolicy::WaitAll
}

/// Parse error policy from node attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ErrorPolicy {
    Continue,
    FailFast,
    Ignore,
}

impl std::fmt::Display for ErrorPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue => write!(f, "continue"),
            Self::FailFast => write!(f, "fail_fast"),
            Self::Ignore => write!(f, "ignore"),
        }
    }
}

fn parse_error_policy(raw: &str) -> ErrorPolicy {
    match raw {
        "fail_fast" => ErrorPolicy::FailFast,
        "ignore" => ErrorPolicy::Ignore,
        _ => ErrorPolicy::Continue,
    }
}

struct BranchResult {
    id: String,
    outcome: Outcome,
}

#[async_trait]
impl Handler for ParallelHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        logs_root: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, ArcError> {
        let parallel_start = Instant::now();
        let branches = graph.outgoing_edges(&node.id);
        if branches.is_empty() {
            return Ok(Outcome::fail("No branches for parallel node"));
        }

        let join_policy = parse_join_policy(
            node.attrs
                .get("join_policy")
                .and_then(|v| v.as_str())
                .unwrap_or("wait_all"),
        );
        let error_policy = parse_error_policy(
            node.attrs
                .get("error_policy")
                .and_then(|v| v.as_str())
                .unwrap_or("continue"),
        );

        services.emitter.emit(&PipelineEvent::ParallelStarted {
            branch_count: branches.len(),
            join_policy: join_policy.to_string(),
            error_policy: error_policy.to_string(),
        });
        let max_parallel = node
            .attrs
            .get("max_parallel")
            .and_then(super::super::graph::types::AttrValue::as_i64)
            .unwrap_or(4);
        let max_parallel = usize::try_from(max_parallel).unwrap_or(4).max(1);

        let semaphore = Arc::new(Semaphore::new(max_parallel));

        // Build branch tasks
        let mut handles = Vec::new();
        for (branch_index, edge) in branches.iter().enumerate() {
            let target_id = edge.to.clone();
            let branch_context = context.clone_context();
            let registry = Arc::clone(&services.registry);
            let emitter = Arc::clone(&services.emitter);
            let execution_env = Arc::clone(&services.execution_env);
            let graph = graph.clone();
            let logs_root = logs_root.to_path_buf();
            let sem = Arc::clone(&semaphore);

            let handle = tokio::spawn(async move {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|e| ArcError::Handler(format!("semaphore error: {e}")))?;

                emitter.emit(&PipelineEvent::ParallelBranchStarted {
                    branch: target_id.clone(),
                    index: branch_index,
                });
                let branch_start = Instant::now();

                let Some(target_node) = graph.nodes.get(&target_id) else {
                    let outcome =
                        Outcome::fail(format!("branch target node not found: {target_id}"));
                    emitter.emit(&PipelineEvent::ParallelBranchCompleted {
                        branch: target_id.clone(),
                        index: branch_index,
                        duration_ms: millis_u64(branch_start.elapsed()),
                        status: "fail".to_string(),
                    });
                    return Ok(BranchResult {
                        id: target_id.clone(),
                        outcome,
                    });
                };

                let branch_services = EngineServices {
                    registry: Arc::clone(&registry),
                    emitter: Arc::clone(&emitter),
                    execution_env: Arc::clone(&execution_env),
                };
                let handler = registry.resolve(target_node);
                let outcome = handler
                    .execute(
                        target_node,
                        &branch_context,
                        &graph,
                        &logs_root,
                        &branch_services,
                    )
                    .await?;

                emitter.emit(&PipelineEvent::ParallelBranchCompleted {
                    branch: target_id.clone(),
                    index: branch_index,
                    duration_ms: millis_u64(branch_start.elapsed()),
                    status: outcome.status.to_string(),
                });

                Ok::<BranchResult, ArcError>(BranchResult {
                    id: target_id,
                    outcome,
                })
            });
            handles.push(handle);
        }

        // Collect results
        let total_branches = handles.len();
        let mut results: Vec<BranchResult> = Vec::new();
        for (handle_index, handle) in handles.into_iter().enumerate() {
            match handle.await {
                Ok(Ok(result)) => {
                    if error_policy == ErrorPolicy::FailFast
                        && result.outcome.status == StageStatus::Fail
                    {
                        results.push(result);
                        services
                            .emitter
                            .emit(&PipelineEvent::ParallelEarlyTermination {
                                reason: "fail_fast_branch_failed".to_string(),
                                completed_count: results.len(),
                                pending_count: total_branches - handle_index - 1,
                            });
                        break;
                    }
                    results.push(result);
                }
                Ok(Err(e)) => {
                    let result = BranchResult {
                        id: String::new(),
                        outcome: Outcome::fail(e.to_string()),
                    };
                    if error_policy == ErrorPolicy::FailFast {
                        results.push(result);
                        services
                            .emitter
                            .emit(&PipelineEvent::ParallelEarlyTermination {
                                reason: "fail_fast_handler_error".to_string(),
                                completed_count: results.len(),
                                pending_count: total_branches - handle_index - 1,
                            });
                        break;
                    }
                    results.push(result);
                }
                Err(join_err) => {
                    let result = BranchResult {
                        id: String::new(),
                        outcome: Outcome::fail(format!("task join error: {join_err}")),
                    };
                    if error_policy == ErrorPolicy::FailFast {
                        results.push(result);
                        services
                            .emitter
                            .emit(&PipelineEvent::ParallelEarlyTermination {
                                reason: "fail_fast_join_error".to_string(),
                                completed_count: results.len(),
                                pending_count: total_branches - handle_index - 1,
                            });
                        break;
                    }
                    results.push(result);
                }
            }
        }

        // Count successes and failures
        let success_count = results
            .iter()
            .filter(|r| r.outcome.status == StageStatus::Success)
            .count();
        let fail_count = results
            .iter()
            .filter(|r| r.outcome.status == StageStatus::Fail)
            .count();
        let total = results.len();

        // Store results as JSON in context for downstream fan-in
        let results_json: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "status": r.outcome.status.to_string(),
                })
            })
            .collect();
        context.set("parallel.results", serde_json::json!(results_json));
        context.set("parallel.branch_count", serde_json::json!(total));

        let visit = crate::engine::visit_from_context(context);
        let node_dir = crate::engine::node_dir(logs_root, &node.id, visit);
        let _ = tokio::fs::create_dir_all(&node_dir).await;
        if let Ok(json) = serde_json::to_string_pretty(&results_json) {
            let _ = tokio::fs::write(node_dir.join("parallel_results.json"), json).await;
        }

        services.emitter.emit(&PipelineEvent::ParallelCompleted {
            duration_ms: millis_u64(parallel_start.elapsed()),
            success_count,
            failure_count: fail_count,
        });

        // Evaluate join policy
        let status = match join_policy {
            JoinPolicy::WaitAll => {
                if fail_count == 0 || error_policy == ErrorPolicy::Ignore {
                    StageStatus::Success
                } else {
                    StageStatus::PartialSuccess
                }
            }
            JoinPolicy::FirstSuccess => {
                if success_count > 0 {
                    StageStatus::Success
                } else {
                    StageStatus::Fail
                }
            }
            JoinPolicy::KOfN(k) => {
                if success_count >= k {
                    StageStatus::Success
                } else {
                    StageStatus::Fail
                }
            }
            JoinPolicy::Quorum(fraction) => {
                let total_f64 = total as f64;
                let threshold_f64 = (fraction * total_f64).ceil();
                let threshold = threshold_f64 as usize;
                if success_count >= threshold {
                    StageStatus::Success
                } else {
                    StageStatus::Fail
                }
            }
        };

        // Build suggested_next_ids from successful branch targets
        let branch_ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();

        let is_fail = status == StageStatus::Fail;
        let mut outcome = Outcome {
            status,
            preferred_label: None,
            suggested_next_ids: branch_ids,
            context_updates: std::collections::HashMap::new(),
            notes: Some(format!(
                "Parallel node dispatched {total} branches ({success_count} succeeded, {fail_count} failed)"
            )),
            failure_reason: if is_fail {
                Some(format!("Join policy not satisfied: {success_count}/{total} succeeded"))
            } else {
                None
            },
            usage: None,
            files_touched: Vec::new(),
        };

        if is_fail {
            outcome.suggested_next_ids.clear();
        }

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventEmitter;
    use crate::graph::{AttrValue, Edge};
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;

    fn make_services() -> EngineServices {
        let registry = HandlerRegistry::new(Box::new(StartHandler));
        EngineServices {
            registry: Arc::new(registry),
            emitter: Arc::new(EventEmitter::new()),
            execution_env: Arc::new(arc_agent::LocalExecutionEnvironment::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
        }
    }

    #[tokio::test]
    async fn parallel_handler_no_branches() {
        let services = make_services();
        let node = Node::new("par");
        let context = Context::new();
        let graph = Graph::new("test");
        let logs_root = Path::new("/tmp/test");

        let outcome = ParallelHandler
            .execute(&node, &context, &graph, logs_root, &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
    }

    #[tokio::test]
    async fn parallel_handler_with_branches() {
        let services = make_services();
        let mut node = Node::new("par");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("component".to_string()),
        );
        let context = Context::new();
        let mut graph = Graph::new("test");
        graph.nodes.insert("par".to_string(), node.clone());
        graph
            .nodes
            .insert("branch_a".to_string(), Node::new("branch_a"));
        graph
            .nodes
            .insert("branch_b".to_string(), Node::new("branch_b"));
        graph.edges.push(Edge::new("par", "branch_a"));
        graph.edges.push(Edge::new("par", "branch_b"));

        let tmp = tempfile::tempdir().unwrap();
        let outcome = ParallelHandler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
        assert!(outcome.notes.as_deref().unwrap().contains("2 branches"));

        // Check context was set
        let results = context.get("parallel.results");
        assert!(results.is_some());

        // Check parallel_results.json was written
        let results_path = tmp
            .path()
            .join("nodes")
            .join("par")
            .join("parallel_results.json");
        assert!(
            results_path.exists(),
            "parallel_results.json should be written"
        );
        let content = std::fs::read_to_string(&results_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            parsed.is_array(),
            "parallel_results.json should be a JSON array"
        );
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn parallel_handler_first_success_policy() {
        let services = make_services();
        let mut node = Node::new("par");
        node.attrs.insert(
            "join_policy".to_string(),
            AttrValue::String("first_success".to_string()),
        );
        let context = Context::new();
        let mut graph = Graph::new("test");
        graph.nodes.insert("par".to_string(), node.clone());
        graph
            .nodes
            .insert("branch_a".to_string(), Node::new("branch_a"));
        graph.edges.push(Edge::new("par", "branch_a"));

        let logs_root = Path::new("/tmp/test");
        let outcome = ParallelHandler
            .execute(&node, &context, &graph, logs_root, &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn parallel_handler_k_of_n_policy() {
        let services = make_services();
        let mut node = Node::new("par");
        node.attrs.insert(
            "join_policy".to_string(),
            AttrValue::String("k_of_n(2)".to_string()),
        );
        let context = Context::new();
        let mut graph = Graph::new("test");
        graph.nodes.insert("par".to_string(), node.clone());
        graph
            .nodes
            .insert("branch_a".to_string(), Node::new("branch_a"));
        graph
            .nodes
            .insert("branch_b".to_string(), Node::new("branch_b"));
        graph
            .nodes
            .insert("branch_c".to_string(), Node::new("branch_c"));
        graph.edges.push(Edge::new("par", "branch_a"));
        graph.edges.push(Edge::new("par", "branch_b"));
        graph.edges.push(Edge::new("par", "branch_c"));

        let logs_root = Path::new("/tmp/test");
        let outcome = ParallelHandler
            .execute(&node, &context, &graph, logs_root, &services)
            .await
            .unwrap();

        // All 3 succeed (default StartHandler returns success), need 2
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[test]
    fn join_policy_display() {
        assert_eq!(JoinPolicy::WaitAll.to_string(), "wait_all");
        assert_eq!(JoinPolicy::FirstSuccess.to_string(), "first_success");
        assert_eq!(JoinPolicy::KOfN(3).to_string(), "k_of_n(3)");
        assert_eq!(JoinPolicy::Quorum(0.5).to_string(), "quorum(0.5)");
    }

    #[test]
    fn error_policy_display() {
        assert_eq!(ErrorPolicy::Continue.to_string(), "continue");
        assert_eq!(ErrorPolicy::FailFast.to_string(), "fail_fast");
        assert_eq!(ErrorPolicy::Ignore.to_string(), "ignore");
    }

    #[test]
    fn parse_join_policy_variants() {
        assert!(matches!(parse_join_policy("wait_all"), JoinPolicy::WaitAll));
        assert!(matches!(
            parse_join_policy("first_success"),
            JoinPolicy::FirstSuccess
        ));
        assert!(matches!(
            parse_join_policy("k_of_n(3)"),
            JoinPolicy::KOfN(3)
        ));
        assert!(matches!(
            parse_join_policy("quorum(0.5)"),
            JoinPolicy::Quorum(_)
        ));
        // Invalid falls back to WaitAll
        assert!(matches!(parse_join_policy("invalid"), JoinPolicy::WaitAll));
    }

    #[test]
    fn parse_error_policy_variants() {
        assert_eq!(parse_error_policy("continue"), ErrorPolicy::Continue);
        assert_eq!(parse_error_policy("fail_fast"), ErrorPolicy::FailFast);
        assert_eq!(parse_error_policy("ignore"), ErrorPolicy::Ignore);
        assert_eq!(parse_error_policy("unknown"), ErrorPolicy::Continue);
    }
}
