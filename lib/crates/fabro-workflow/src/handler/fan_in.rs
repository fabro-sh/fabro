use std::path::Path;
use std::sync::Arc;

use crate::context::Context;
use crate::context::keys;
use crate::error::FabroError;
use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::outcome::{Outcome, OutcomeExt};
use crate::run_dir::visit_from_context;
use crate::sandbox_git::git_merge_ff_only;
use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_graphviz::graph::{Graph, Node};

use super::agent::{CodergenBackend, CodergenResult};
use super::{EngineServices, Handler};

/// Consolidates results from a preceding parallel node and selects the best candidate.
pub struct FanInHandler {
    backend: Option<Box<dyn CodergenBackend>>,
}

impl FanInHandler {
    #[must_use]
    pub fn new(backend: Option<Box<dyn CodergenBackend>>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Handler for FanInHandler {
    async fn simulate(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        let results = context.get(keys::PARALLEL_RESULTS);
        let Some(results) = results else {
            return Ok(Outcome::fail_deterministic(
                "No parallel results to evaluate",
            ));
        };

        let best = heuristic_select(&results);

        let mut outcome = Outcome::simulated(&node.id);
        outcome.context_updates.insert(
            keys::PARALLEL_FAN_IN_BEST_ID.to_string(),
            serde_json::json!(best.id),
        );
        outcome.context_updates.insert(
            keys::PARALLEL_FAN_IN_BEST_OUTCOME.to_string(),
            serde_json::json!(best.status),
        );
        // Override the generic simulated notes with handler-specific detail.
        outcome.notes = Some(format!("[Simulated] Selected best candidate: {}", best.id));
        Ok(outcome)
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        let results = context.get(keys::PARALLEL_RESULTS);
        let Some(results) = results else {
            return Ok(Outcome::fail_deterministic(
                "No parallel results to evaluate",
            ));
        };

        let prompt = node.prompt().filter(|p| !p.is_empty());

        let best = if let (Some(prompt_text), Some(backend)) = (prompt, &self.backend) {
            llm_evaluate(
                backend.as_ref(),
                prompt_text,
                &results,
                context,
                run_dir,
                &node.id,
                &services.emitter,
                &services.sandbox,
            )
            .await?
        } else {
            heuristic_select(&results)
        };

        // Check if all candidates failed — if so, return fail
        let all_failed = if best.status == "fail" {
            let empty_vec = vec![];
            let arr = results.as_array().unwrap_or(&empty_vec);
            arr.iter()
                .all(|v| v.get("status").and_then(|v| v.as_str()).unwrap_or("fail") == "fail")
        } else {
            false
        };

        if all_failed {
            return Ok(Outcome::fail_deterministic("all candidates failed"));
        }

        // --- Fast-forward to winner's HEAD when git isolation is active ---
        let best_head_sha = {
            let empty_vec = vec![];
            let arr = results.as_array().unwrap_or(&empty_vec);
            arr.iter()
                .find(|v| v.get("id").and_then(|v| v.as_str()) == Some(&best.id))
                .and_then(|v| v.get("head_sha").and_then(|v| v.as_str()).map(String::from))
        };

        if let (Some(ref sha), Some(_)) = (&best_head_sha, services.git_state()) {
            git_merge_ff_only(&*services.sandbox, sha).await;
        }

        let mut outcome = Outcome::success();
        outcome.context_updates.insert(
            keys::PARALLEL_FAN_IN_BEST_ID.to_string(),
            serde_json::json!(best.id),
        );
        outcome.context_updates.insert(
            keys::PARALLEL_FAN_IN_BEST_OUTCOME.to_string(),
            serde_json::json!(best.status),
        );
        if let Some(ref sha) = best_head_sha {
            outcome.context_updates.insert(
                keys::PARALLEL_FAN_IN_BEST_HEAD_SHA.to_string(),
                serde_json::json!(sha),
            );
        }
        outcome.notes = Some(format!("Selected best candidate: {}", best.id));

        Ok(outcome)
    }
}

struct Candidate {
    id: String,
    status: String,
    score: f64,
}

fn status_rank(status: &str) -> u32 {
    match status {
        "success" => 0,
        "partial_success" => 1,
        "retry" => 2,
        "fail" => 3,
        _ => 4,
    }
}

fn heuristic_select(results: &serde_json::Value) -> Candidate {
    let empty_vec = vec![];
    let arr = results.as_array().unwrap_or(&empty_vec);
    if arr.is_empty() {
        return Candidate {
            id: "unknown".to_string(),
            status: "fail".to_string(),
            score: 0.0,
        };
    }

    let mut candidates: Vec<Candidate> = arr
        .iter()
        .map(|v| Candidate {
            id: v
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            status: v
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("fail")
                .to_string(),
            score: v
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
        })
        .collect();

    candidates.sort_by(|a, b| {
        let rank_cmp = status_rank(&a.status).cmp(&status_rank(&b.status));
        if rank_cmp != std::cmp::Ordering::Equal {
            return rank_cmp;
        }
        // Higher score is better, so reverse the comparison
        let score_cmp = b
            .score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal);
        if score_cmp != std::cmp::Ordering::Equal {
            return score_cmp;
        }
        a.id.cmp(&b.id)
    });

    candidates.into_iter().next().unwrap_or_else(|| Candidate {
        id: "unknown".to_string(),
        status: "fail".to_string(),
        score: 0.0,
    })
}

/// Use an LLM backend to evaluate and rank parallel branch results.
#[allow(clippy::too_many_arguments)]
async fn llm_evaluate(
    backend: &dyn CodergenBackend,
    prompt: &str,
    results: &serde_json::Value,
    context: &Context,
    _run_dir: &Path,
    node_id: &str,
    emitter: &Arc<EventEmitter>,
    sandbox: &Arc<dyn Sandbox>,
) -> Result<Candidate, FabroError> {
    let results_text =
        serde_json::to_string_pretty(results).unwrap_or_else(|_| results.to_string());

    let full_prompt = format!(
        "{prompt}\n\nParallel branch results:\n{results_text}\n\n\
         Respond with the ID of the best candidate."
    );

    let visit_u32 = u32::try_from(visit_from_context(context)).unwrap_or(u32::MAX);

    emitter.emit(&WorkflowRunEvent::Prompt {
        stage: node_id.to_string(),
        visit: visit_u32,
        text: full_prompt.clone(),
        mode: Some("fan_in".to_string()),
        provider: None,
        model: None,
    });

    // Build a synthetic node for the backend call
    let eval_node = Node::new("fan_in_eval");

    // Fan-in evaluation runs outside a thread context, so pass None
    match backend
        .run(
            &eval_node,
            &full_prompt,
            context,
            None,
            emitter,
            sandbox,
            None,
        )
        .await
    {
        Ok(CodergenResult::Full(outcome)) => {
            // If the backend returned a full Outcome, extract best_id from context_updates
            let best_id = outcome
                .context_updates
                .get(keys::PARALLEL_FAN_IN_BEST_ID)
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| outcome.notes.clone())
                .unwrap_or_else(|| "unknown".to_string());
            let response_text =
                serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".to_string());
            emitter.emit(&WorkflowRunEvent::PromptCompleted {
                node_id: node_id.to_string(),
                response: response_text.clone(),
                model: String::new(),
                provider: String::new(),
                usage: None,
            });
            Ok(Candidate {
                id: best_id,
                status: outcome.status.to_string(),
                score: 0.0,
            })
        }
        Ok(CodergenResult::Text { text, .. }) => {
            emitter.emit(&WorkflowRunEvent::PromptCompleted {
                node_id: node_id.to_string(),
                response: text.clone(),
                model: String::new(),
                provider: String::new(),
                usage: None,
            });

            // The LLM responded with text; try to find a matching candidate ID
            let text = text.trim().to_string();
            let empty_vec = vec![];
            let arr = results.as_array().unwrap_or(&empty_vec);

            // Check if the response text matches any candidate ID
            for v in arr {
                if let Some(id) = v.get("id").and_then(|v| v.as_str()) {
                    if text.contains(id) {
                        let status = v
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("success")
                            .to_string();
                        let score = v
                            .get("score")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.0);
                        return Ok(Candidate {
                            id: id.to_string(),
                            status,
                            score,
                        });
                    }
                }
            }

            // No match found; fall back to heuristic
            Ok(heuristic_select(results))
        }
        Err(_) => {
            // LLM call failed; fall back to heuristic
            Ok(heuristic_select(results))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::StageStatus;

    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    #[tokio::test]
    async fn fan_in_no_results() {
        let handler = FanInHandler::new(None);
        let node = Node::new("fan_in");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
    }

    #[tokio::test]
    async fn fan_in_selects_best() {
        let handler = FanInHandler::new(None);
        let node = Node::new("fan_in");
        let context = Context::new();
        context.set(
            keys::PARALLEL_RESULTS,
            serde_json::json!([
                {"id": "branch_a", "status": "fail"},
                {"id": "branch_b", "status": "success"},
            ]),
        );
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        assert_eq!(
            outcome.context_updates.get(keys::PARALLEL_FAN_IN_BEST_ID),
            Some(&serde_json::json!("branch_b"))
        );
    }

    #[tokio::test]
    async fn fan_in_lexical_tiebreak() {
        let handler = FanInHandler::new(None);
        let node = Node::new("fan_in");
        let context = Context::new();
        context.set(
            keys::PARALLEL_RESULTS,
            serde_json::json!([
                {"id": "c", "status": "success"},
                {"id": "a", "status": "success"},
                {"id": "b", "status": "success"},
            ]),
        );
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(
            outcome.context_updates.get(keys::PARALLEL_FAN_IN_BEST_ID),
            Some(&serde_json::json!("a"))
        );
    }

    #[test]
    fn status_rank_ordering() {
        assert!(status_rank("success") < status_rank("partial_success"));
        assert!(status_rank("partial_success") < status_rank("retry"));
        assert!(status_rank("retry") < status_rank("fail"));
    }

    #[tokio::test]
    async fn fan_in_no_backend_ignores_prompt() {
        // When there's a prompt but no backend, it should fall back to heuristic
        let handler = FanInHandler::new(None);
        let mut node = Node::new("fan_in");
        node.attrs.insert(
            "prompt".to_string(),
            fabro_graphviz::graph::AttrValue::String("Pick the best branch".to_string()),
        );
        let context = Context::new();
        context.set(
            keys::PARALLEL_RESULTS,
            serde_json::json!([
                {"id": "branch_a", "status": "success"},
                {"id": "branch_b", "status": "fail"},
            ]),
        );
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        // Should still pick branch_a via heuristic (success beats fail)
        assert_eq!(
            outcome.context_updates.get(keys::PARALLEL_FAN_IN_BEST_ID),
            Some(&serde_json::json!("branch_a"))
        );
    }

    #[tokio::test]
    async fn fan_in_with_backend_llm_eval() {
        use crate::handler::agent::CodergenBackend;
        use tempfile::TempDir;

        struct MockBackend;

        #[async_trait]
        impl CodergenBackend for MockBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<EventEmitter>,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                // Return text that contains the ID "branch_b"
                Ok(CodergenResult::Text {
                    text: "The best candidate is branch_b".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
                })
            }
        }

        let handler = FanInHandler::new(Some(Box::new(MockBackend)));
        let mut node = Node::new("fan_in");
        node.attrs.insert(
            "prompt".to_string(),
            fabro_graphviz::graph::AttrValue::String("Pick the best branch".to_string()),
        );
        let context = Context::new();
        context.set(
            keys::PARALLEL_RESULTS,
            serde_json::json!([
                {"id": "branch_a", "status": "success"},
                {"id": "branch_b", "status": "success"},
            ]),
        );
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        // LLM chose branch_b
        assert_eq!(
            outcome.context_updates.get(keys::PARALLEL_FAN_IN_BEST_ID),
            Some(&serde_json::json!("branch_b"))
        );
    }

    #[tokio::test]
    async fn fan_in_all_fail_returns_fail() {
        let handler = FanInHandler::new(None);
        let node = Node::new("fan_in");
        let context = Context::new();
        context.set(
            keys::PARALLEL_RESULTS,
            serde_json::json!([
                {"id": "branch_a", "status": "fail"},
                {"id": "branch_b", "status": "fail"},
                {"id": "branch_c", "status": "fail"},
            ]),
        );
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(
            outcome
                .failure_reason()
                .unwrap()
                .contains("all candidates failed")
        );
    }

    #[tokio::test]
    async fn fan_in_score_tiebreak() {
        let handler = FanInHandler::new(None);
        let node = Node::new("fan_in");
        let context = Context::new();
        context.set(
            keys::PARALLEL_RESULTS,
            serde_json::json!([
                {"id": "branch_a", "status": "success", "score": 0.5},
                {"id": "branch_b", "status": "success", "score": 0.9},
                {"id": "branch_c", "status": "success", "score": 0.7},
            ]),
        );
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        // branch_b has highest score
        assert_eq!(
            outcome.context_updates.get(keys::PARALLEL_FAN_IN_BEST_ID),
            Some(&serde_json::json!("branch_b"))
        );
    }

    #[tokio::test]
    async fn fan_in_simulate_uses_heuristic() {
        let handler = FanInHandler::new(None);
        let node = Node::new("fan_in");
        let context = Context::new();
        context.set(
            keys::PARALLEL_RESULTS,
            serde_json::json!([
                {"id": "branch_a", "status": "fail"},
                {"id": "branch_b", "status": "success"},
            ]),
        );
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .simulate(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        assert!(outcome.notes.as_deref().unwrap().contains("[Simulated]"));
        assert_eq!(
            outcome.context_updates.get(keys::PARALLEL_FAN_IN_BEST_ID),
            Some(&serde_json::json!("branch_b"))
        );
    }
}
