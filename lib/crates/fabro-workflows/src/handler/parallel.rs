use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use fabro_agent::{Sandbox, WorktreeConfig, WorktreeSandbox};
use tokio::sync::Semaphore;

use crate::context::keys;
use crate::context::{Context, WorkflowContext};
use crate::engine::set_hook_node;
use crate::error::FabroError;
use crate::event::WorkflowRunEvent;
use crate::millis_u64;
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use fabro_graphviz::graph::{Graph, Node};
use fabro_hooks::{HookContext, HookEvent};

use super::{EngineServices, Handler};

/// Fans out execution to multiple branches concurrently.
/// Each branch gets an isolated context clone and runs independently.
pub struct ParallelHandler;

/// Parse join policy from node attributes.
#[derive(Debug, Clone)]
enum JoinPolicy {
    WaitAll,
    FirstSuccess,
}

impl std::fmt::Display for JoinPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WaitAll => write!(f, "wait_all"),
            Self::FirstSuccess => write!(f, "first_success"),
        }
    }
}

fn parse_join_policy(raw: &str) -> JoinPolicy {
    if raw == "first_success" {
        return JoinPolicy::FirstSuccess;
    }
    JoinPolicy::WaitAll
}

struct BranchResult {
    id: String,
    outcome: Outcome,
    head_sha: Option<String>,
    worktree_path: Option<PathBuf>,
}

#[async_trait]
impl Handler for ParallelHandler {
    async fn simulate(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        let branches = graph.outgoing_edges(&node.id);
        if branches.is_empty() {
            return Ok(Outcome::fail_classify("No branches for parallel node"));
        }

        // Dispatch each branch child via dispatch_handler (which will call simulate)
        let mut branch_results: Vec<BranchResult> = Vec::new();
        for edge in &branches {
            let target_id = &edge.to;
            if let Some(target_node) = graph.nodes.get(target_id) {
                let handler = services.registry.resolve(target_node);
                let branch_context = context.fork();
                let outcome = super::dispatch_handler(
                    handler,
                    target_node,
                    &branch_context,
                    graph,
                    run_dir,
                    services,
                )
                .await?;
                branch_results.push(BranchResult {
                    id: target_id.clone(),
                    outcome,
                    head_sha: None,
                    worktree_path: None,
                });
            }
        }

        let total = branch_results.len();
        context.set(keys::PARALLEL_BRANCH_COUNT, serde_json::json!(total));

        let results_json: Vec<serde_json::Value> = branch_results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "status": r.outcome.status.to_string(),
                })
            })
            .collect();
        context.set(keys::PARALLEL_RESULTS, serde_json::json!(results_json));

        let join_node = find_join_node(&branch_results, graph);

        let mut outcome = Outcome::simulated(&node.id);
        outcome.notes = Some(format!(
            "[Simulated] Parallel node dispatched {total} branches"
        ));
        outcome.jump_to_node = join_node;
        Ok(outcome)
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        let parallel_start = Instant::now();
        let branches = graph.outgoing_edges(&node.id);
        if branches.is_empty() {
            return Ok(Outcome::fail_classify("No branches for parallel node"));
        }

        let join_policy = parse_join_policy(
            node.attrs
                .get("join_policy")
                .and_then(|v| v.as_str())
                .unwrap_or("wait_all"),
        );

        services.emitter.emit(&WorkflowRunEvent::ParallelStarted {
            branch_count: branches.len(),
            join_policy: join_policy.to_string(),
        });
        {
            let mut hook_ctx = HookContext::new(
                HookEvent::ParallelStart,
                context.run_id(),
                graph.name.clone(),
            );
            set_hook_node(&mut hook_ctx, node);
            let _ = services.run_hooks(&hook_ctx).await;
        }
        let max_parallel = node
            .attrs
            .get("max_parallel")
            .and_then(fabro_graphviz::graph::AttrValue::as_i64)
            .unwrap_or(4);
        let max_parallel = usize::try_from(max_parallel).unwrap_or(4).max(1);

        let semaphore = Arc::new(Semaphore::new(max_parallel));
        let git_state = services.git_state();

        // --- Git isolation: checkpoint "parallel base" before fan-out ---
        let base_sha: Option<String> = if let Some(ref gs) = git_state {
            let result = crate::engine::git_checkpoint(
                &*services.sandbox,
                &gs.run_id,
                &node.id,
                "parallel_base",
                0,
                None,
                &gs.checkpoint_exclude_globs,
                &gs.git_author,
            )
            .await;
            match result {
                Ok(sha) => Some(sha),
                Err(e) => {
                    tracing::warn!(error = %e, "parallel base checkpoint failed");
                    None
                }
            }
        } else {
            None
        };

        // Build per-branch sandboxes (sequentially for git setup)
        struct BranchSetup {
            target_id: String,
            branch_index: usize,
            branch_context: Context,
            sandbox: Arc<dyn Sandbox>,
            worktree_path: Option<PathBuf>,
        }

        let mut branch_setups: Vec<BranchSetup> = Vec::new();
        for (branch_index, edge) in branches.iter().enumerate() {
            let target_id = edge.to.clone();
            let branch_context = context.fork();

            let (branch_sandbox, worktree_path): (Arc<dyn Sandbox>, Option<PathBuf>) = if let (
                Some(ref gs),
                Some(ref bsha),
            ) =
                (&git_state, &base_sha)
            {
                let branch_key = &target_id;
                let visit = crate::engine::visit_from_context(&branch_context);
                let branch_name = format!(
                    "fabro/run/parallel/{}/{}/pass{}/{}",
                    gs.run_id,
                    crate::git::sanitize_ref_component(&node.id),
                    visit,
                    crate::git::sanitize_ref_component(branch_key),
                );

                // Compute worktree path (each sandbox type knows its own path scheme)
                let wt_path_str = services
                    .sandbox
                    .parallel_worktree_path(run_dir, &gs.run_id, &node.id, branch_key);
                tracing::debug!(branch = %branch_name, path = %wt_path_str, "Creating worktree for parallel branch");

                // Set up worktree via WorktreeSandbox
                let wt_config = WorktreeConfig {
                    branch_name: branch_name.clone(),
                    base_sha: bsha.clone(),
                    worktree_path: wt_path_str.clone(),
                    skip_branch_creation: false,
                };
                let mut wt_sandbox = WorktreeSandbox::new(Arc::clone(&services.sandbox), wt_config);
                wt_sandbox.set_event_callback(Arc::clone(&services.emitter).worktree_callback());
                wt_sandbox
                    .initialize()
                    .await
                    .map_err(|e| FabroError::handler(format!("worktree setup failed: {e}")))?;

                branch_context.set(keys::INTERNAL_WORK_DIR, serde_json::json!(&wt_path_str));

                let wt_path = PathBuf::from(&wt_path_str);
                let env: Arc<dyn Sandbox> = Arc::new(wt_sandbox);
                (env, Some(wt_path))
            } else {
                (Arc::clone(&services.sandbox), None)
            };

            branch_setups.push(BranchSetup {
                target_id,
                branch_index,
                branch_context,
                sandbox: branch_sandbox,
                worktree_path,
            });
        }

        // --- Fan out: concurrent execution ---
        let mut handles = Vec::new();
        for setup in branch_setups {
            let registry = Arc::clone(&services.registry);
            let emitter = Arc::clone(&services.emitter);
            let hook_runner = services.hook_runner.clone();
            let env = services.env.clone();
            let dry_run = services.dry_run;
            let graph = graph.clone();
            let run_dir = run_dir.to_path_buf();
            let sem = Arc::clone(&semaphore);
            let has_git = git_state.is_some();
            let run_id = git_state.as_ref().map(|gs| gs.run_id.clone());
            let git_author = git_state
                .as_ref()
                .map(|gs| gs.git_author.clone())
                .unwrap_or_default();

            let handle = tokio::spawn(async move {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|e| FabroError::handler(format!("semaphore error: {e}")))?;

                emitter.emit(&WorkflowRunEvent::ParallelBranchStarted {
                    branch: setup.target_id.clone(),
                    index: setup.branch_index,
                });
                let branch_start = Instant::now();

                let Some(target_node) = graph.nodes.get(&setup.target_id) else {
                    let outcome = Outcome::fail_classify(format!(
                        "branch target node not found: {}",
                        setup.target_id
                    ));
                    emitter.emit(&WorkflowRunEvent::ParallelBranchCompleted {
                        branch: setup.target_id.clone(),
                        index: setup.branch_index,
                        duration_ms: millis_u64(branch_start.elapsed()),
                        status: "fail".to_string(),
                    });
                    return Ok(BranchResult {
                        id: setup.target_id.clone(),
                        outcome,
                        head_sha: None,
                        worktree_path: setup.worktree_path,
                    });
                };

                let branch_services = EngineServices {
                    registry: Arc::clone(&registry),
                    emitter: Arc::clone(&emitter),
                    sandbox: Arc::clone(&setup.sandbox),
                    git_state: std::sync::RwLock::new(None),
                    hook_runner: hook_runner.clone(),
                    env: env.clone(),
                    dry_run,
                };
                let handler = registry.resolve(target_node);
                let outcome = super::dispatch_handler(
                    handler,
                    target_node,
                    &setup.branch_context,
                    &graph,
                    &run_dir,
                    &branch_services,
                )
                .await?;

                // Checkpoint commit after branch execution (capture head_sha)
                let head_sha = if has_git {
                    let rid = run_id.as_deref().unwrap_or("unknown");
                    let nid = &setup.target_id;
                    let status_str = outcome.status.to_string();
                    // Use exec_command to commit and capture HEAD in the branch worktree
                    let git_r = crate::engine::GIT_REMOTE;
                    let add_cmd = format!("{git_r} add -A");
                    let add_result = setup
                        .sandbox
                        .exec_command(&add_cmd, 30_000, None, None, None)
                        .await;
                    if add_result.as_ref().is_ok_and(|r| r.exit_code == 0) {
                        let msg = format!("fabro({rid}): {nid} ({status_str})");
                        let commit_cmd = format!(
                            "{git_r} -c 'user.name={name}' -c 'user.email={email}' commit --allow-empty -m '{msg}'",
                            name = git_author.name,
                            email = git_author.email,
                        );
                        let _ = setup
                            .sandbox
                            .exec_command(&commit_cmd, 30_000, None, None, None)
                            .await;
                    }
                    let sha_cmd = format!("{git_r} rev-parse HEAD");
                    let sha_result = setup
                        .sandbox
                        .exec_command(&sha_cmd, 10_000, None, None, None)
                        .await;
                    match sha_result {
                        Ok(r) if r.exit_code == 0 => {
                            let sha = r.stdout.trim().to_string();
                            emitter.emit(&WorkflowRunEvent::GitCommit {
                                node_id: Some(setup.target_id.clone()),
                                sha: sha.clone(),
                            });
                            Some(sha)
                        }
                        _ => None,
                    }
                } else {
                    None
                };

                emitter.emit(&WorkflowRunEvent::ParallelBranchCompleted {
                    branch: setup.target_id.clone(),
                    index: setup.branch_index,
                    duration_ms: millis_u64(branch_start.elapsed()),
                    status: outcome.status.to_string(),
                });

                Ok::<BranchResult, FabroError>(BranchResult {
                    id: setup.target_id,
                    outcome,
                    head_sha,
                    worktree_path: setup.worktree_path,
                })
            });
            handles.push(handle);
        }

        // Collect results
        let mut results: Vec<BranchResult> = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(Ok(result)) => {
                    results.push(result);
                }
                Ok(Err(e)) => {
                    results.push(BranchResult {
                        id: String::new(),
                        outcome: e.to_fail_outcome(),
                        head_sha: None,
                        worktree_path: None,
                    });
                }
                Err(join_err) => {
                    results.push(BranchResult {
                        id: String::new(),
                        outcome: Outcome::fail_classify(format!("task join error: {join_err}")),
                        head_sha: None,
                        worktree_path: None,
                    });
                }
            }
        }

        // --- Git isolation: clean up worktrees, then ff-merge winner ---
        if git_state.is_some() {
            // Clean up worktrees first
            for result in &results {
                if let Some(ref wt_path) = result.worktree_path {
                    let wt_str = wt_path.to_string_lossy().into_owned();
                    crate::engine::git_remove_worktree(&*services.sandbox, &wt_str).await;
                    services
                        .emitter
                        .emit(&WorkflowRunEvent::GitWorktreeRemove { path: wt_str });
                }
            }

            // Fast-forward main branch to first successful branch (lexically sorted).
            // This must happen here — before the engine creates its own checkpoint commit
            // on the main branch — so that subsequent commits are descendants of the winner.
            let mut successful: Vec<_> = results
                .iter()
                .filter(|r| r.outcome.status == StageStatus::Success && r.head_sha.is_some())
                .collect();
            successful.sort_by(|a, b| a.id.cmp(&b.id));
            if let Some(winner) = successful.first() {
                let sha = winner.head_sha.as_ref().unwrap();
                crate::engine::git_merge_ff_only(&*services.sandbox, sha).await;
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
                let mut entry = serde_json::json!({
                    "id": r.id,
                    "status": r.outcome.status.to_string(),
                });
                if let Some(ref sha) = r.head_sha {
                    entry["head_sha"] = serde_json::json!(sha);
                }
                entry
            })
            .collect();
        context.set(keys::PARALLEL_RESULTS, serde_json::json!(results_json));
        context.set(keys::PARALLEL_BRANCH_COUNT, serde_json::json!(total));

        let visit = crate::engine::visit_from_context(context);
        let node_dir = crate::engine::node_dir(run_dir, &node.id, visit);
        let _ = tokio::fs::create_dir_all(&node_dir).await;
        if let Ok(json) = serde_json::to_string_pretty(&results_json) {
            let _ = tokio::fs::write(node_dir.join("parallel_results.json"), json).await;
        }

        services.emitter.emit(&WorkflowRunEvent::ParallelCompleted {
            duration_ms: millis_u64(parallel_start.elapsed()),
            success_count,
            failure_count: fail_count,
        });
        {
            let mut hook_ctx = HookContext::new(
                HookEvent::ParallelComplete,
                context.run_id(),
                graph.name.clone(),
            );
            set_hook_node(&mut hook_ctx, node);
            let _ = services.run_hooks(&hook_ctx).await;
        }

        // Evaluate join policy
        let status = match join_policy {
            JoinPolicy::WaitAll => {
                if fail_count == 0 {
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
        };

        // Find the join/convergence node: follow each branch's outgoing edges
        // and find the common downstream target (typically the fan-in node).
        let join_node = find_join_node(&results, graph);

        let is_fail = status == StageStatus::Fail;
        let mut outcome = Outcome {
            status,
            notes: Some(format!(
                "Parallel node dispatched {total} branches ({success_count} succeeded, {fail_count} failed)"
            )),
            failure: if is_fail {
                Some(crate::outcome::FailureDetail::new(
                    format!("Join policy not satisfied: {success_count}/{total} succeeded"),
                    crate::outcome::FailureCategory::Deterministic,
                ))
            } else {
                None
            },
            jump_to_node: if is_fail { None } else { join_node },
            ..Outcome::success()
        };

        if is_fail {
            outcome.suggested_next_ids.clear();
        }

        Ok(outcome)
    }
}

/// Find the convergence (join/fan-in) node by following each branch's outgoing edges
/// and finding the first node reachable from all branches.
fn find_join_node(results: &[BranchResult], graph: &Graph) -> Option<String> {
    if results.is_empty() {
        return None;
    }

    // Collect outgoing targets for each branch
    let mut target_sets: Vec<std::collections::HashSet<String>> = Vec::new();
    for result in results {
        let targets: std::collections::HashSet<String> = graph
            .outgoing_edges(&result.id)
            .into_iter()
            .map(|e| e.to.clone())
            .collect();
        target_sets.push(targets);
    }

    // Find the intersection — nodes reachable from ALL branches
    let first = target_sets.first()?;
    let common: std::collections::HashSet<&String> = first
        .iter()
        .filter(|id| target_sets.iter().all(|set| set.contains(*id)))
        .collect();

    // Return the first common target (lexically sorted for determinism)
    let mut common_sorted: Vec<&String> = common.into_iter().collect();
    common_sorted.sort();
    common_sorted.first().map(|id| (*id).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_graphviz::graph::{AttrValue, Edge};

    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    #[tokio::test]
    async fn parallel_handler_no_branches() {
        let services = make_services();
        let node = Node::new("par");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");

        let outcome = ParallelHandler
            .execute(&node, &context, &graph, run_dir, &services)
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
        let results = context.get(keys::PARALLEL_RESULTS);
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

        let run_dir = Path::new("/tmp/test");
        let outcome = ParallelHandler
            .execute(&node, &context, &graph, run_dir, &services)
            .await
            .unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[test]
    fn join_policy_display() {
        assert_eq!(JoinPolicy::WaitAll.to_string(), "wait_all");
        assert_eq!(JoinPolicy::FirstSuccess.to_string(), "first_success");
    }

    #[test]
    fn parse_join_policy_variants() {
        assert!(matches!(parse_join_policy("wait_all"), JoinPolicy::WaitAll));
        assert!(matches!(
            parse_join_policy("first_success"),
            JoinPolicy::FirstSuccess
        ));
        // Invalid falls back to WaitAll
        assert!(matches!(parse_join_policy("invalid"), JoinPolicy::WaitAll));
    }

    #[tokio::test]
    async fn parallel_handler_simulate() {
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
        // Add a fan_in node reachable from both branches
        graph
            .nodes
            .insert("fan_in".to_string(), Node::new("fan_in"));
        graph.edges.push(Edge::new("par", "branch_a"));
        graph.edges.push(Edge::new("par", "branch_b"));
        graph.edges.push(Edge::new("branch_a", "fan_in"));
        graph.edges.push(Edge::new("branch_b", "fan_in"));

        let run_dir = Path::new("/tmp/test");
        let mut dry_services = services;
        dry_services.dry_run = true;

        let outcome = ParallelHandler
            .simulate(&node, &context, &graph, run_dir, &dry_services)
            .await
            .unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
        assert!(outcome.notes.as_deref().unwrap().contains("[Simulated]"));
        assert!(outcome.notes.as_deref().unwrap().contains("2 branches"));
        assert_eq!(outcome.jump_to_node, Some("fan_in".to_string()));

        let branch_count = context.get(keys::PARALLEL_BRANCH_COUNT);
        assert_eq!(branch_count, Some(serde_json::json!(2)));
    }
}
