use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use arc_agent::ExecutionEnvironment;
use async_trait::async_trait;
use tokio::sync::Semaphore;

use crate::context::Context;
use crate::engine::GitCheckpointMode;
use crate::error::ArcError;
use crate::event::PipelineEvent;
use crate::graph::{Graph, Node};
use crate::outcome::{Outcome, StageStatus};

use super::{EngineServices, Handler};

// ---------------------------------------------------------------------------
// WorktreeEnv — decorates an ExecutionEnvironment with a custom working dir
// ---------------------------------------------------------------------------

/// Wraps an existing `ExecutionEnvironment` so that all operations use a
/// different working directory (the worktree path inside a remote sandbox).
struct WorktreeEnv {
    inner: Arc<dyn ExecutionEnvironment>,
    worktree_dir: String,
}

#[async_trait]
impl ExecutionEnvironment for WorktreeEnv {
    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        self.inner.read_file(path, offset, limit).await
    }
    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        self.inner.write_file(path, content).await
    }
    async fn delete_file(&self, path: &str) -> Result<(), String> {
        self.inner.delete_file(path).await
    }
    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        self.inner.file_exists(path).await
    }
    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> Result<Vec<arc_agent::execution_env::DirEntry>, String> {
        self.inner.list_directory(path, depth).await
    }
    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&std::collections::HashMap<String, String>>,
        cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<arc_agent::execution_env::ExecResult, String> {
        // Default to worktree dir when no explicit working_dir is given
        let wd = working_dir.unwrap_or(&self.worktree_dir);
        self.inner
            .exec_command(command, timeout_ms, Some(wd), env_vars, cancel_token)
            .await
    }
    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &arc_agent::execution_env::GrepOptions,
    ) -> Result<Vec<String>, String> {
        self.inner.grep(pattern, path, options).await
    }
    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
        self.inner.glob(pattern, path).await
    }
    async fn initialize(&self) -> Result<(), String> {
        self.inner.initialize().await
    }
    async fn cleanup(&self) -> Result<(), String> {
        self.inner.cleanup().await
    }
    fn working_directory(&self) -> &str {
        &self.worktree_dir
    }
    fn platform(&self) -> &str {
        self.inner.platform()
    }
    fn os_version(&self) -> String {
        self.inner.os_version()
    }
}

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
    head_sha: Option<String>,
    worktree_path: Option<PathBuf>,
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
        let git_state = services.git_state();

        // --- Git isolation: checkpoint "parallel base" before fan-out ---
        let base_sha: Option<String> = if let Some(ref gs) = git_state {
            match &gs.mode {
                GitCheckpointMode::Host(work_dir) => {
                    let wd = work_dir.clone();
                    let rid = gs.run_id.clone();
                    let nid = node.id.clone();
                    crate::engine::git_checkpoint_host(
                        wd,
                        rid,
                        nid,
                        "parallel_base".into(),
                        0,
                        None,
                    )
                    .await
                }
                GitCheckpointMode::Remote(_) => {
                    crate::engine::git_checkpoint_remote(
                        &*services.execution_env,
                        &gs.run_id,
                        &node.id,
                        "parallel_base",
                        0,
                        None,
                    )
                    .await
                }
            }
        } else {
            None
        };

        // Build per-branch execution environments (sequentially for git setup)
        struct BranchSetup {
            target_id: String,
            branch_index: usize,
            branch_context: Context,
            execution_env: Arc<dyn ExecutionEnvironment>,
            worktree_path: Option<PathBuf>,
        }

        let mut branch_setups: Vec<BranchSetup> = Vec::new();
        for (branch_index, edge) in branches.iter().enumerate() {
            let target_id = edge.to.clone();
            let branch_context = context.clone_context();

            let (branch_exec_env, worktree_path): (Arc<dyn ExecutionEnvironment>, Option<PathBuf>) =
                if let (Some(ref gs), Some(ref bsha)) = (&git_state, &base_sha) {
                    let branch_key = &target_id;
                    let branch_name = format!(
                        "arc/run/parallel/{}/{}/{}",
                        gs.run_id,
                        crate::git::sanitize_ref_component(&node.id),
                        crate::git::sanitize_ref_component(branch_key),
                    );

                    match &gs.mode {
                        GitCheckpointMode::Host(work_dir) => {
                            let wt_path = logs_root
                                .join("parallel")
                                .join(&node.id)
                                .join(branch_key)
                                .join("worktree");
                            let wd = work_dir.clone();
                            let bn = branch_name.clone();
                            let bs = bsha.clone();
                            let wtp = wt_path.clone();
                            tokio::task::spawn_blocking(move || {
                                crate::git::create_branch_at(&wd, &bn, &bs)?;
                                crate::git::replace_worktree(&wd, &wtp, &bn)?;
                                crate::git::reset_hard(&wtp, &bs)
                            })
                            .await
                            .map_err(|e| {
                                ArcError::Handler(format!("worktree setup join error: {e}"))
                            })??;
                            branch_context.set(
                                "internal.work_dir",
                                serde_json::json!(wt_path.to_string_lossy().as_ref()),
                            );
                            let env: Arc<dyn ExecutionEnvironment> = Arc::new(
                                arc_agent::LocalExecutionEnvironment::new(wt_path.clone()),
                            );
                            (env, Some(wt_path))
                        }
                        GitCheckpointMode::Remote(_) => {
                            let wt_path_str = format!(
                                "/home/daytona/workspace/.arc-parallel/{}/{}",
                                node.id, branch_key
                            );
                            let ok = crate::engine::git_create_branch_at_remote(
                                &*services.execution_env,
                                &branch_name,
                                bsha,
                            )
                            .await;
                            if !ok {
                                return Err(ArcError::Handler(format!(
                                    "failed to create remote branch {branch_name}"
                                )));
                            }
                            let ok = crate::engine::git_replace_worktree_remote(
                                &*services.execution_env,
                                &wt_path_str,
                                &branch_name,
                            )
                            .await;
                            if !ok {
                                return Err(ArcError::Handler(format!(
                                    "failed to add remote worktree {wt_path_str}"
                                )));
                            }
                            // Reset worktree to the base SHA for a clean start
                            let reset_cmd =
                                format!("{} reset --hard {bsha}", crate::engine::GIT_REMOTE);
                            let reset_result = services
                                .execution_env
                                .exec_command(&reset_cmd, 30_000, Some(&wt_path_str), None, None)
                                .await;
                            if !matches!(reset_result, Ok(ref r) if r.exit_code == 0) {
                                return Err(ArcError::Handler(format!(
                                    "failed to reset remote worktree {wt_path_str}"
                                )));
                            }
                            branch_context
                                .set("internal.work_dir", serde_json::json!(&wt_path_str));
                            let env: Arc<dyn ExecutionEnvironment> = Arc::new(WorktreeEnv {
                                inner: Arc::clone(&services.execution_env),
                                worktree_dir: wt_path_str.clone(),
                            });
                            (env, Some(PathBuf::from(wt_path_str)))
                        }
                    }
                } else {
                    (Arc::clone(&services.execution_env), None)
                };

            branch_setups.push(BranchSetup {
                target_id,
                branch_index,
                branch_context,
                execution_env: branch_exec_env,
                worktree_path,
            });
        }

        // --- Fan out: concurrent execution ---
        let mut handles = Vec::new();
        for setup in branch_setups {
            let registry = Arc::clone(&services.registry);
            let emitter = Arc::clone(&services.emitter);
            let graph = graph.clone();
            let logs_root = logs_root.to_path_buf();
            let sem = Arc::clone(&semaphore);
            let has_git = git_state.is_some();
            let run_id = git_state.as_ref().map(|gs| gs.run_id.clone());

            let handle = tokio::spawn(async move {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|e| ArcError::Handler(format!("semaphore error: {e}")))?;

                emitter.emit(&PipelineEvent::ParallelBranchStarted {
                    branch: setup.target_id.clone(),
                    index: setup.branch_index,
                });
                let branch_start = Instant::now();

                let Some(target_node) = graph.nodes.get(&setup.target_id) else {
                    let outcome =
                        Outcome::fail(format!("branch target node not found: {}", setup.target_id));
                    emitter.emit(&PipelineEvent::ParallelBranchCompleted {
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
                    execution_env: Arc::clone(&setup.execution_env),
                    git_state: std::sync::RwLock::new(None),
                };
                let handler = registry.resolve(target_node);
                let outcome = handler
                    .execute(
                        target_node,
                        &setup.branch_context,
                        &graph,
                        &logs_root,
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
                        .execution_env
                        .exec_command(&add_cmd, 30_000, None, None, None)
                        .await;
                    if add_result.as_ref().is_ok_and(|r| r.exit_code == 0) {
                        let msg = format!("arc({rid}): {nid} ({status_str})");
                        let commit_cmd = format!(
                            "{git_r} -c user.name=arc -c user.email=arc@local commit --allow-empty -m '{msg}'"
                        );
                        let _ = setup
                            .execution_env
                            .exec_command(&commit_cmd, 30_000, None, None, None)
                            .await;
                    }
                    let sha_cmd = format!("{git_r} rev-parse HEAD");
                    let sha_result = setup
                        .execution_env
                        .exec_command(&sha_cmd, 10_000, None, None, None)
                        .await;
                    match sha_result {
                        Ok(r) if r.exit_code == 0 => Some(r.stdout.trim().to_string()),
                        _ => None,
                    }
                } else {
                    None
                };

                emitter.emit(&PipelineEvent::ParallelBranchCompleted {
                    branch: setup.target_id.clone(),
                    index: setup.branch_index,
                    duration_ms: millis_u64(branch_start.elapsed()),
                    status: outcome.status.to_string(),
                });

                Ok::<BranchResult, ArcError>(BranchResult {
                    id: setup.target_id,
                    outcome,
                    head_sha,
                    worktree_path: setup.worktree_path,
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
                        head_sha: None,
                        worktree_path: None,
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
                        head_sha: None,
                        worktree_path: None,
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

        // --- Git isolation: clean up worktrees, then ff-merge winner ---
        if let Some(ref gs) = git_state {
            // Clean up worktrees first
            for result in &results {
                if let Some(ref wt_path) = result.worktree_path {
                    match &gs.mode {
                        GitCheckpointMode::Host(work_dir) => {
                            let wd = work_dir.clone();
                            let wtp = wt_path.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                crate::git::remove_worktree(&wd, &wtp)
                            })
                            .await;
                        }
                        GitCheckpointMode::Remote(_) => {
                            let wt_str = wt_path.to_string_lossy().to_string();
                            crate::engine::git_remove_worktree_remote(
                                &*services.execution_env,
                                &wt_str,
                            )
                            .await;
                        }
                    }
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
                match &gs.mode {
                    GitCheckpointMode::Host(work_dir) => {
                        let wd = work_dir.clone();
                        let s = sha.clone();
                        let _ =
                            tokio::task::spawn_blocking(move || crate::git::merge_ff_only(&wd, &s))
                                .await;
                    }
                    GitCheckpointMode::Remote(_) => {
                        crate::engine::git_merge_ff_only_remote(&*services.execution_env, sha)
                            .await;
                    }
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

        // Build suggested_next_ids from branch targets
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
            git_state: std::sync::RwLock::new(None),
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
