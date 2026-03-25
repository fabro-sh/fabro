use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use fabro_agent::Sandbox;
use fabro_core::executor::ExecutorBuilder;
use fabro_core::state::RunState;
use fabro_util::backoff::BackoffPolicy;
use rand::Rng;
use tokio_util::sync::CancellationToken;

use fabro_git_storage::trailerlink::{self, Trailer};

use crate::asset_snapshot;
use crate::checkpoint::Checkpoint;
use crate::condition::evaluate_condition;
use crate::context;
use crate::context::Context;
use crate::error::{FabroError, FailureCategory, Result};
use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::handler::{EngineServices, HandlerRegistry};
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use fabro_config::{config::FabroConfig, run::PullRequestConfig};
use fabro_graphviz::graph::{Edge, Graph, Node};
use fabro_hooks::{HookContext, HookDecision, HookEvent, HookRunner};
use fabro_interview::Interviewer;

/// Populate node-related fields on a `HookContext` from a graph `Node`.
pub(crate) fn set_hook_node(ctx: &mut HookContext, node: &Node) {
    ctx.node_id = Some(node.id.clone());
    ctx.node_label = Some(node.label().to_string());
    ctx.handler_type = node.handler_type().map(String::from);
}

/// Classify the failure mode of a completed outcome.
///
/// Returns `None` for `Success`, `PartialSuccess`, and `Skipped` outcomes.
/// For failures, checks (in priority order):
/// 1. Handler hint in `context_updates["failure_class"]`
/// 2. String heuristics on `failure_reason`
/// 3. Default to `Deterministic`
#[must_use]
pub(crate) fn classify_outcome(outcome: &Outcome) -> Option<FailureCategory> {
    match outcome.status {
        StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped => None,
        StageStatus::Fail | StageStatus::Retry => outcome
            .failure_category()
            .or(Some(FailureCategory::Deterministic)),
    }
}

// --- Retry policy types ---

/// Retry policy for node execution.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffPolicy,
}

impl RetryPolicy {
    const DEFAULT_BACKOFF: BackoffPolicy = BackoffPolicy {
        initial_delay: Duration::from_millis(5_000),
        factor: 2.0,
        max_delay: Duration::from_millis(60_000),
        jitter: true,
    };

    /// No retries -- fail immediately.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            backoff: Self::DEFAULT_BACKOFF,
        }
    }

    /// Standard retry policy: 5 attempts, 5s initial, 2x factor.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            max_attempts: 5,
            backoff: Self::DEFAULT_BACKOFF,
        }
    }

    /// Aggressive retry: 5 attempts, 500ms initial, 2x factor.
    #[must_use]
    pub fn aggressive() -> Self {
        Self {
            max_attempts: 5,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_millis(500),
                ..Self::DEFAULT_BACKOFF
            },
        }
    }

    /// Linear retry: 3 attempts, 500ms fixed delay.
    #[must_use]
    pub fn linear() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_millis(500),
                factor: 1.0,
                ..Self::DEFAULT_BACKOFF
            },
        }
    }

    /// Patient retry: 3 attempts, 2000ms initial, 3x factor.
    #[must_use]
    pub fn patient() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffPolicy {
                initial_delay: Duration::from_millis(2000),
                factor: 3.0,
                ..Self::DEFAULT_BACKOFF
            },
        }
    }
}

/// Build a retry policy from node and graph attributes.
/// If the node has a `retry_policy` attribute naming a preset, use that.
/// Otherwise, fall back to `max_retries` / graph default.
pub(crate) fn build_retry_policy(node: &Node, graph: &Graph) -> RetryPolicy {
    if let Some(preset) = node.retry_policy() {
        match preset {
            "none" => return RetryPolicy::none(),
            "standard" => return RetryPolicy::standard(),
            "aggressive" => return RetryPolicy::aggressive(),
            "linear" => return RetryPolicy::linear(),
            "patient" => return RetryPolicy::patient(),
            _ => {} // Unknown preset, fall through to max_retries behavior
        }
    }
    let max_retries = node
        .max_retries()
        .unwrap_or_else(|| graph.default_max_retries());
    // max_retries=0 means 1 attempt (no retries)
    let max_attempts = u32::try_from(max_retries + 1).unwrap_or(1).max(1);
    RetryPolicy {
        max_attempts,
        backoff: RetryPolicy::DEFAULT_BACKOFF,
    }
}

// --- Fidelity resolution (spec 5.4) ---

/// Resolve the context fidelity for a node, following the precedence:
/// 1. Incoming edge `fidelity` attribute
/// 2. Target node `fidelity` attribute
/// 3. Graph `default_fidelity` attribute
/// 4. Default: Compact
#[must_use]
pub fn resolve_fidelity(
    incoming_edge: Option<&Edge>,
    node: &Node,
    graph: &Graph,
) -> context::keys::Fidelity {
    let (resolved, source) = if let Some(f) = incoming_edge
        .and_then(|e| e.fidelity())
        .and_then(|s| s.parse().ok())
    {
        (f, "edge")
    } else if let Some(f) = node.fidelity().and_then(|s| s.parse().ok()) {
        (f, "node")
    } else if let Some(f) = graph.default_fidelity().and_then(|s| s.parse().ok()) {
        (f, "graph")
    } else {
        (context::keys::Fidelity::default(), "default")
    };

    tracing::debug!(
        node = %node.id,
        fidelity = %resolved,
        source = source,
        "Fidelity resolved"
    );

    resolved
}

// --- Thread ID resolution (spec 5.4) ---

/// Resolve the thread ID for a node, following the precedence:
/// 1. Incoming edge `thread_id` attribute
/// 2. Target node `thread_id` attribute
/// 3. Graph-level default thread
/// 4. Derived class from enclosing subgraph (first class from the node's classes list)
/// 5. Fallback to previous node ID
#[must_use]
pub fn resolve_thread_id(
    incoming_edge: Option<&Edge>,
    node: &Node,
    graph: &Graph,
    previous_node_id: Option<&str>,
) -> Option<String> {
    // Step 1: Edge thread_id
    if let Some(edge) = incoming_edge {
        if let Some(tid) = edge.thread_id() {
            return Some(tid.to_string());
        }
    }
    // Step 2: Node thread_id
    if let Some(tid) = node.thread_id() {
        return Some(tid.to_string());
    }
    // Step 3: Graph-level default thread
    if let Some(tid) = graph.default_thread() {
        return Some(tid.to_string());
    }
    // Step 4: Derived class from enclosing subgraph
    if let Some(first_class) = node.classes.first() {
        return Some(first_class.clone());
    }
    // Step 5: Fallback to previous node ID
    previous_node_id.map(String::from)
}

// --- Run directory helpers (spec 5.6) ---

/// Write start.json at the start of a workflow run. Returns the StartRecord.
pub(crate) fn write_start_record(
    run_dir: &Path,
    settings: &RunSettings,
) -> crate::start_record::StartRecord {
    let git_state = settings.git.as_ref();
    let record = crate::start_record::StartRecord {
        run_id: settings.run_id.clone(),
        start_time: Utc::now(),
        run_branch: git_state.and_then(|g| g.run_branch.clone()),
        base_sha: git_state.and_then(|g| g.base_sha.clone()),
    };
    let _ = std::fs::create_dir_all(run_dir);
    let _ = record.save(run_dir);
    record
}

/// Return the directory for a node's logs.
///
/// First visit (`visit <= 1`): `{run_dir}/nodes/{node_id}`
/// Subsequent visits: `{run_dir}/nodes/{node_id}-visit_{visit}`
pub fn node_dir(run_dir: &Path, node_id: &str, visit: usize) -> PathBuf {
    if visit <= 1 {
        run_dir.join("nodes").join(node_id)
    } else {
        run_dir
            .join("nodes")
            .join(format!("{node_id}-visit_{visit}"))
    }
}

/// Read the workflow visit ordinal from context.
///
/// The raw context value is `0` when unset; workflow execution code treats
/// missing counts as the first visit for stage/log naming.
pub fn visit_from_context(context: &Context) -> usize {
    context.node_visit_count().max(1)
}

/// Write status.json for a completed node into {`run_dir}/nodes/{node_id}/status.json`.
pub(crate) fn write_node_status(run_dir: &Path, node_id: &str, visit: usize, outcome: &Outcome) {
    let node_dir = node_dir(run_dir, node_id, visit);
    let _ = std::fs::create_dir_all(&node_dir);
    let status = serde_json::json!({
        "status": outcome.status.to_string(),
        "notes": outcome.notes,
        "failure_reason": outcome.failure_reason(),
        "timestamp": Utc::now().to_rfc3339(),
    });
    if let Ok(json) = serde_json::to_string_pretty(&status) {
        let _ = std::fs::write(node_dir.join("status.json"), json);
    }
}

// --- Edge selection ---

/// Normalize a label for comparison: lowercase, trim, strip accelerator prefixes.
/// Patterns: "[Y] ", "Y) ", "Y - "
fn normalize_label(label: &str) -> String {
    let s = label.trim().to_lowercase();
    // Strip "[X] " prefix
    if s.starts_with('[') {
        if let Some(rest) = s
            .strip_prefix('[')
            .and_then(|s| s.find(']').map(|i| s[i + 1..].trim_start().to_string()))
        {
            return rest;
        }
    }
    // Strip "X) " prefix
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if bytes.get(1) == Some(&b')') {
            return s[2..].trim_start().to_string();
        }
    }
    // Strip "X - " prefix
    if s.len() >= 3 {
        if let Some(rest) = s.get(1..).and_then(|r| r.strip_prefix(" - ")) {
            return rest.to_string();
        }
    }
    s
}

/// Pick the best edge by highest weight, then lexical target node ID tiebreak.
fn best_by_weight_then_lexical<'a>(edges: &[&'a Edge]) -> Option<&'a Edge> {
    if edges.is_empty() {
        return None;
    }
    let mut best = edges[0];
    for &edge in &edges[1..] {
        if edge.weight() > best.weight() || (edge.weight() == best.weight() && edge.to < best.to) {
            best = edge;
        }
    }
    Some(best)
}

/// Pick a random edge using weighted-random selection.
/// Edges with `weight <= 0` are treated as weight 1 for probability calculation.
fn weighted_random<'a>(edges: &[&'a Edge]) -> Option<&'a Edge> {
    if edges.is_empty() {
        return None;
    }
    if edges.len() == 1 {
        return Some(edges[0]);
    }
    let weights: Vec<f64> = edges
        .iter()
        .map(|e| {
            let w = e.weight();
            if w <= 0 {
                1.0
            } else {
                w as f64
            }
        })
        .collect();
    let total: f64 = weights.iter().sum();
    let mut rng = rand::thread_rng();
    let mut roll: f64 = rng.gen_range(0.0..total);
    for (i, &w) in weights.iter().enumerate() {
        roll -= w;
        if roll < 0.0 {
            return Some(edges[i]);
        }
    }
    Some(edges[edges.len() - 1])
}

/// Dispatch to the appropriate edge-picking strategy.
fn pick_edge<'a>(edges: &[&'a Edge], selection: &str) -> Option<&'a Edge> {
    match selection {
        "random" => weighted_random(edges),
        _ => best_by_weight_then_lexical(edges),
    }
}

/// Select the next edge from a node's outgoing edges (spec Section 3.3).
#[must_use]
/// Result of edge selection: the chosen edge and the reason it was selected.
pub struct EdgeSelection<'a> {
    pub edge: &'a Edge,
    pub reason: &'static str,
}

fn blocks_unconditional_failure_fallthrough(node: &Node, outcome: &Outcome) -> bool {
    node.handler_type() == Some("human")
        && outcome.status == StageStatus::Fail
        && outcome.preferred_label.is_none()
        && outcome.suggested_next_ids.is_empty()
}

pub fn select_edge<'a>(
    node: &Node,
    outcome: &Outcome,
    context: &Context,
    graph: &'a Graph,
    selection: &str,
) -> Option<EdgeSelection<'a>> {
    let node_id = &node.id;
    let edges = graph.outgoing_edges(node_id);
    if edges.is_empty() {
        return None;
    }

    // Step 1: Condition matching
    let condition_matched: Vec<&Edge> = edges
        .iter()
        .filter(|e| {
            e.condition()
                .is_some_and(|c| !c.is_empty() && evaluate_condition(c, outcome, context))
        })
        .copied()
        .collect();
    if !condition_matched.is_empty() {
        return pick_edge(&condition_matched, selection).map(|edge| EdgeSelection {
            edge,
            reason: "condition",
        });
    }

    // Step 2: Preferred label match (unconditional edges only)
    if let Some(pref) = &outcome.preferred_label {
        let normalized_pref = normalize_label(pref);
        for edge in &edges {
            if edge.condition().is_none_or(str::is_empty) {
                if let Some(label) = edge.label() {
                    if normalize_label(label) == normalized_pref {
                        return Some(EdgeSelection {
                            edge,
                            reason: "preferred_label",
                        });
                    }
                }
            }
        }
    }

    // Step 3: Suggested next IDs (unconditional edges only)
    for suggested_id in &outcome.suggested_next_ids {
        for edge in &edges {
            if edge.condition().is_none_or(str::is_empty) && edge.to == *suggested_id {
                return Some(EdgeSelection {
                    edge,
                    reason: "suggested_next",
                });
            }
        }
    }

    if blocks_unconditional_failure_fallthrough(node, outcome) {
        return None;
    }

    // Step 4 & 5: Weight with lexical tiebreak (unconditional edges only)
    let unconditional: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.condition().is_none_or(str::is_empty))
        .copied()
        .collect();
    if !unconditional.is_empty() {
        return pick_edge(&unconditional, selection).map(|edge| EdgeSelection {
            edge,
            reason: "unconditional",
        });
    }

    None
}

// --- Goal gate enforcement ---

/// Check if all goal gates have been satisfied.
/// Returns Ok(()) if all gates passed, or Err with the failed node ID.
pub(crate) fn check_goal_gates(
    graph: &Graph,
    node_outcomes: &HashMap<String, Outcome>,
) -> std::result::Result<(), String> {
    for (node_id, outcome) in node_outcomes {
        if let Some(node) = graph.nodes.get(node_id) {
            if node.goal_gate()
                && outcome.status != StageStatus::Success
                && outcome.status != StageStatus::PartialSuccess
            {
                return Err(node_id.clone());
            }
        }
    }
    Ok(())
}

/// Resolve the retry target for a failed goal gate node.
pub(crate) fn get_retry_target(failed_node_id: &str, graph: &Graph) -> Option<String> {
    if let Some(node) = graph.nodes.get(failed_node_id) {
        // Node-level retry_target
        if let Some(target) = node.retry_target() {
            if graph.nodes.contains_key(target) {
                return Some(target.to_string());
            }
        }
        // Node-level fallback_retry_target
        if let Some(target) = node.fallback_retry_target() {
            if graph.nodes.contains_key(target) {
                return Some(target.to_string());
            }
        }
    }
    // Graph-level retry_target
    if let Some(target) = graph.retry_target() {
        if graph.nodes.contains_key(target) {
            return Some(target.to_string());
        }
    }
    // Graph-level fallback_retry_target
    if let Some(target) = graph.fallback_retry_target() {
        if graph.nodes.contains_key(target) {
            return Some(target.to_string());
        }
    }
    None
}

/// Check whether a node is a terminal (exit) node.
pub(crate) fn is_terminal(node: &Node) -> bool {
    node.shape() == "Msquare" || node.handler_type() == Some("exit")
}

pub(crate) fn node_script(node: &Node) -> Option<String> {
    node.attrs
        .get("script")
        .or_else(|| node.attrs.get("tool_command"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

// --- Workflow run engine ---

/// Captured git state for a workflow run, shared with handlers.
#[derive(Debug, Clone)]
pub struct GitState {
    pub run_id: String,
    pub base_sha: String,
    pub run_branch: Option<String>,
    pub meta_branch: Option<String>,
    pub checkpoint_exclude_globs: Vec<String>,
    pub git_author: crate::git::GitAuthor,
}

pub const GIT_REMOTE: &str = "git -c maintenance.auto=0 -c gc.auto=0";

/// Shell-escape a string using `shlex::try_quote` (POSIX-safe).
fn shell_quote(s: &str) -> String {
    shlex::try_quote(s).map_or_else(
        |_| format!("'{}'", s.replace('\'', "'\\''")),
        |q| q.to_string(),
    )
}

/// Run a git checkpoint commit via the sandbox.
#[allow(clippy::too_many_arguments)]
pub async fn git_checkpoint(
    sandbox: &dyn Sandbox,
    run_id: &str,
    node_id: &str,
    status: &str,
    completed_count: usize,
    shadow_sha: Option<String>,
    exclude_globs: &[String],
    author: &crate::git::GitAuthor,
) -> std::result::Result<String, String> {
    // Stage everything, always excluding EXCLUDE_DIRS plus any user-configured globs
    let mut all_excludes: Vec<String> = asset_snapshot::EXCLUDE_DIRS
        .iter()
        .map(|d| format!("**/{d}/**"))
        .collect();
    all_excludes.extend(exclude_globs.iter().cloned());

    let pathspecs: Vec<String> = all_excludes
        .iter()
        .map(|g| format!("':(glob,exclude){g}'"))
        .collect();
    let add_cmd = format!("{GIT_REMOTE} add -A -- . {}", pathspecs.join(" "));
    let add_result = sandbox
        .exec_command(&add_cmd, 30_000, None, None, None)
        .await;
    match &add_result {
        Ok(r) if r.exit_code == 0 => {}
        Ok(r) => {
            return Err(format!(
                "git add failed (exit {}): {}{}",
                r.exit_code, r.stdout, r.stderr
            ));
        }
        Err(e) => return Err(format!("git add failed: {e}")),
    }

    // Build commit message with trailers (same format as checkpoint_commit in git.rs)
    let subject = format!("fabro({run_id}): {node_id} ({status})");
    let completed_str = completed_count.to_string();
    let mut trailers = vec![
        Trailer {
            key: "Fabro-Run",
            value: run_id,
        },
        Trailer {
            key: "Fabro-Completed",
            value: &completed_str,
        },
    ];
    let shadow_sha_ref = shadow_sha.as_deref().unwrap_or("");
    if shadow_sha.is_some() {
        trailers.push(Trailer {
            key: "Fabro-Checkpoint",
            value: shadow_sha_ref,
        });
    }
    let mut message = trailerlink::format_message(&subject, "", &trailers);
    author.append_footer(&mut message);

    // Write message to a unique temp file to avoid races between concurrent local runs
    let msg_path = format!("/tmp/fabro-commit-msg-{run_id}-{node_id}");
    if let Err(e) = sandbox.write_file(&msg_path, &message).await {
        return Err(format!("failed to write commit message file: {e}"));
    }

    // Commit with configured identity using the message file
    let commit_cmd = format!(
        "{GIT_REMOTE} -c user.name={name} -c user.email={email} commit --allow-empty -F {msg_path}",
        name = shell_quote(&author.name),
        email = shell_quote(&author.email),
    );
    let commit_result = sandbox
        .exec_command(&commit_cmd, 30_000, None, None, None)
        .await;
    match &commit_result {
        Ok(r) if r.exit_code == 0 => {}
        Ok(r) => {
            return Err(format!(
                "git commit failed (exit {}): {}{}",
                r.exit_code, r.stdout, r.stderr
            ));
        }
        Err(e) => return Err(format!("git commit failed: {e}")),
    }

    // Get the new HEAD SHA
    let sha_cmd = format!("{GIT_REMOTE} rev-parse HEAD");
    let sha_result = sandbox
        .exec_command(&sha_cmd, 10_000, None, None, None)
        .await;
    match sha_result {
        Ok(r) if r.exit_code == 0 => Ok(r.stdout.trim().to_string()),
        Ok(r) => Err(format!(
            "git rev-parse HEAD failed (exit {}): {}{}",
            r.exit_code, r.stdout, r.stderr
        )),
        Err(e) => Err(format!("git rev-parse HEAD failed: {e}")),
    }
}

/// Push a refspec from the host repo to origin (best-effort).
///
/// Authenticates via a GitHub App installation token so we don't depend
/// on the host's ambient git credentials.
pub async fn git_push_host(
    repo_path: &Path,
    refspec: &str,
    github_app: &Option<fabro_github::GitHubAppCredentials>,
    label: &str,
) -> bool {
    let (origin_url, _) = match fabro_sandbox::daytona::detect_repo_info(repo_path) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(error = %e, label, "Cannot detect origin for push");
            return false;
        }
    };

    let https_url = fabro_github::ssh_url_to_https(&origin_url);
    let push_url = match github_app {
        Some(creds) => match fabro_github::resolve_authenticated_url(creds, &https_url).await {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!(error = %e, label, "Failed to get token for push");
                return false;
            }
        },
        None => {
            tracing::warn!(label, "No GitHub App credentials for push");
            return false;
        }
    };

    let rp = repo_path.to_path_buf();
    let refspec_owned = refspec.to_string();
    let result = crate::git::blocking_push_with_timeout(60, move || {
        crate::git::push_ref(&rp, &push_url, &refspec_owned)
    })
    .await;
    match result {
        Ok(()) => {
            tracing::info!(label, "Pushed to origin");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, label, "Failed to push");
            false
        }
    }
}

/// Run a git diff via the sandbox.
pub(crate) async fn git_diff(
    sandbox: &dyn Sandbox,
    base: &str,
) -> std::result::Result<String, String> {
    let cmd = format!("{GIT_REMOTE} diff {base} HEAD");
    match sandbox.exec_command(&cmd, 30_000, None, None, None).await {
        Ok(r) if r.exit_code == 0 => Ok(r.stdout),
        Ok(r) => Err(format!("exit {}: {}", r.exit_code, r.stderr.trim())),
        Err(e) => Err(e.to_string()),
    }
}

// --- Sandbox git helpers ---

/// Create a branch at a specific SHA via the sandbox.
pub async fn git_create_branch_at(sandbox: &dyn Sandbox, name: &str, sha: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} branch --force {name} {sha}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Add a git worktree via the sandbox.
pub async fn git_add_worktree(sandbox: &dyn Sandbox, path: &str, branch: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} worktree add {path} {branch}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Remove a git worktree via the sandbox.
pub async fn git_remove_worktree(sandbox: &dyn Sandbox, path: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} worktree remove --force {path}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Fast-forward merge to a given SHA via the sandbox.
pub async fn git_merge_ff_only(sandbox: &dyn Sandbox, sha: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} merge --ff-only {sha}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Remove any stale worktree at `path` (best-effort), then add a fresh one.
pub async fn git_replace_worktree(sandbox: &dyn Sandbox, path: &str, branch: &str) -> bool {
    let _ = git_remove_worktree(sandbox, path).await;
    git_add_worktree(sandbox, path, branch).await
}

/// Configuration for a workflow run.
#[derive(Clone)]
pub struct GitCheckpointSettings {
    pub base_sha: Option<String>,
    pub run_branch: Option<String>,
    pub meta_branch: Option<String>,
}

/// Configuration for a workflow run.
#[derive(Clone)]
pub struct RunSettings {
    pub config: FabroConfig,
    pub run_dir: PathBuf,
    pub cancel_token: Option<Arc<AtomicBool>>,
    pub dry_run: bool,
    /// Unique identifier for this workflow run.
    pub run_id: String,
    /// User-defined key-value labels for this run.
    pub labels: HashMap<String, String>,
    /// Git author identity for checkpoint commits.
    pub git_author: crate::git::GitAuthor,
    /// Workflow directory slug (e.g. "smoke" from `fabro/workflows/smoke/`).
    pub workflow_slug: Option<String>,
    /// GitHub App credentials for pushing metadata branches to origin.
    pub github_app: Option<fabro_github::GitHubAppCredentials>,
    /// Host repo path for MetadataStore (shadow commits) and host-side pushes.
    pub host_repo_path: Option<PathBuf>,
    /// Name of the branch the run was started from (for PR base).
    pub base_branch: Option<String>,
    /// Git checkpoint settings; `None` means checkpointing disabled.
    pub git: Option<GitCheckpointSettings>,
}

impl RunSettings {
    pub fn checkpoint_exclude_globs(&self) -> &[String] {
        &self.config.checkpoint.exclude_globs
    }

    /// PR config (already normalized — disabled entries stripped at construction).
    pub fn pull_request(&self) -> Option<&PullRequestConfig> {
        self.config.pull_request.as_ref()
    }

    pub fn asset_globs(&self) -> &[String] {
        self.config
            .assets
            .as_ref()
            .map(|a| a.include.as_slice())
            .unwrap_or(&[])
    }
}

/// Configuration for sandbox lifecycle management within the engine.
pub struct LifecycleConfig {
    /// Setup commands to run inside the sandbox after initialization.
    pub setup_commands: Vec<String>,
    /// Timeout in milliseconds for each setup command.
    pub setup_command_timeout_ms: u64,
    /// Devcontainer lifecycle phases and their commands.
    pub devcontainer_phases: Vec<(String, Vec<fabro_devcontainer::Command>)>,
}

/// The workflow run execution engine.
pub struct WorkflowRunEngine {
    services: EngineServices,
    pub interviewer: Option<Arc<dyn Interviewer>>,
}

impl WorkflowRunEngine {
    #[must_use]
    pub fn new(
        registry: HandlerRegistry,
        emitter: Arc<EventEmitter>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            services: EngineServices {
                registry: Arc::new(registry),
                emitter,
                sandbox,
                git_state: std::sync::RwLock::new(None),
                hook_runner: None,
                env: HashMap::new(),
                dry_run: false,
            },
            interviewer: None,
        }
    }

    /// Create a child engine that shares a parent's `Arc` services (registry, emitter, env).
    #[must_use]
    pub fn from_services(services: &EngineServices) -> Self {
        Self {
            services: EngineServices {
                registry: Arc::clone(&services.registry),
                emitter: Arc::clone(&services.emitter),
                sandbox: Arc::clone(&services.sandbox),
                git_state: std::sync::RwLock::new(None),
                hook_runner: services.hook_runner.clone(),
                env: services.env.clone(),
                dry_run: services.dry_run,
            },
            interviewer: None,
        }
    }

    /// Create a new engine with an interviewer for `inform()` callbacks.
    #[must_use]
    pub fn with_interviewer(
        registry: HandlerRegistry,
        emitter: Arc<EventEmitter>,
        interviewer: Arc<dyn Interviewer>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            services: EngineServices {
                registry: Arc::new(registry),
                emitter,
                sandbox,
                git_state: std::sync::RwLock::new(None),
                hook_runner: None,
                env: HashMap::new(),
                dry_run: false,
            },
            interviewer: Some(interviewer),
        }
    }

    /// Set the hook runner for lifecycle hooks.
    pub fn set_hook_runner(&mut self, runner: Arc<HookRunner>) {
        self.services.hook_runner = Some(runner);
    }

    /// Set environment variables from `[sandbox.env]` config.
    pub fn set_env(&mut self, env: HashMap<String, String>) {
        self.services.env = env;
    }

    /// Enable dry-run mode so handlers skip real execution.
    pub fn set_dry_run(&mut self, dry_run: bool) {
        self.services.dry_run = dry_run;
    }

    /// Run lifecycle hooks and return the merged decision.
    /// Returns `Proceed` if no hook runner is configured.
    async fn run_hooks(&self, hook_context: &HookContext, work_dir: Option<&Path>) -> HookDecision {
        let Some(ref runner) = self.services.hook_runner else {
            return HookDecision::Proceed;
        };
        runner
            .run(hook_context, self.services.sandbox.clone(), work_dir)
            .await
    }

    /// Execute the workflow graph. Returns the final outcome.
    ///
    /// # Errors
    ///
    /// Returns an error if no start node is found, a node is missing, or a goal gate fails
    /// without a retry target.
    pub async fn run(&self, graph: &Graph, settings: &RunSettings) -> Result<Outcome> {
        let (outcome, _context) = self.run_via_core(graph, settings, None, None).await?;
        Ok(outcome)
    }

    /// Run a workflow with full sandbox lifecycle management.
    ///
    /// 1. Initialize sandbox
    /// 2. Fire `SandboxReady` hook (blocking — can abort run)
    /// 3. Emit `SandboxInitialized` event
    /// 4. Sandbox git setup via `sandbox.setup_git_for_run()`
    /// 5. Run setup commands
    /// 6. Run devcontainer lifecycle phases
    /// 7. Execute the workflow graph
    ///
    /// The sandbox is left alive after return so the caller can run retro, PR creation, etc.
    /// Call `cleanup_sandbox()` when done.
    ///
    /// The config is taken by mutable reference so the caller retains ownership
    /// and can read any fields mutated by remote git setup after the call.
    pub async fn run_with_lifecycle(
        &self,
        graph: &Graph,
        settings: &mut RunSettings,
        lifecycle: LifecycleConfig,
        checkpoint: Option<&Checkpoint>,
    ) -> Result<Outcome> {
        self.prepare_sandbox(graph, settings, lifecycle).await?;
        self.execute_graph(graph, settings, checkpoint).await
    }

    /// INITIALIZE: sandbox setup, git, setup commands, devcontainer.
    /// Mutates config (fills base_sha, run_branch from sandbox git setup).
    pub async fn prepare_sandbox(
        &self,
        graph: &Graph,
        settings: &mut RunSettings,
        lifecycle: LifecycleConfig,
    ) -> Result<()> {
        // 1. Initialize sandbox
        self.services
            .sandbox
            .initialize()
            .await
            .map_err(|e| FabroError::engine(format!("Failed to initialize sandbox: {e}")))?;

        // 2. Fire SandboxReady hook (blocking — can abort run)
        {
            let hook_ctx = HookContext::new(
                HookEvent::SandboxReady,
                settings.run_id.clone(),
                graph.name.clone(),
            );
            let decision = self.run_hooks(&hook_ctx, None).await;
            if let HookDecision::Block { reason } = decision {
                let msg = reason.unwrap_or_else(|| "blocked by SandboxReady hook".into());
                return Err(FabroError::engine(msg));
            }
        }

        // 3. Emit SandboxInitialized event
        self.services
            .emitter
            .emit(&WorkflowRunEvent::SandboxInitialized {
                working_directory: self.services.sandbox.working_directory().to_string(),
            });

        // 4. Sandbox git setup — let the sandbox set up its own git state if needed.
        //    Skip when caller already has an assigned run branch.
        let has_run_branch = settings
            .git
            .as_ref()
            .and_then(|g| g.run_branch.as_ref())
            .is_some();
        if !has_run_branch {
            match self
                .services
                .sandbox
                .setup_git_for_run(&settings.run_id)
                .await
            {
                Ok(Some(info)) => {
                    let base_sha = settings
                        .git
                        .as_ref()
                        .and_then(|g| g.base_sha.clone())
                        .or(Some(info.base_sha));
                    settings.git = Some(GitCheckpointSettings {
                        base_sha,
                        run_branch: Some(info.run_branch.clone()),
                        meta_branch: Some(crate::git::MetadataStore::branch_name(&settings.run_id)),
                    });
                    if settings.base_branch.is_none() {
                        settings.base_branch = info.base_branch;
                    }
                }
                Ok(None) => {
                    // Sandbox does not manage git internally (e.g. local sandbox)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Sandbox git setup failed, running without git checkpoints");
                }
            }
        }

        // 5. Run setup commands
        if !lifecycle.setup_commands.is_empty() {
            self.services.emitter.emit(&WorkflowRunEvent::SetupStarted {
                command_count: lifecycle.setup_commands.len(),
            });
            let setup_start = Instant::now();
            for (index, cmd) in lifecycle.setup_commands.iter().enumerate() {
                self.services
                    .emitter
                    .emit(&WorkflowRunEvent::SetupCommandStarted {
                        command: cmd.clone(),
                        index,
                    });
                let cmd_start = Instant::now();
                let result = self
                    .services
                    .sandbox
                    .exec_command(cmd, lifecycle.setup_command_timeout_ms, None, None, None)
                    .await
                    .map_err(|e| FabroError::engine(format!("Setup command failed: {e}")))?;
                let cmd_duration = crate::millis_u64(cmd_start.elapsed());
                if result.exit_code != 0 {
                    self.services.emitter.emit(&WorkflowRunEvent::SetupFailed {
                        command: cmd.clone(),
                        index,
                        exit_code: result.exit_code,
                        stderr: result.stderr.clone(),
                    });
                    return Err(FabroError::engine(format!(
                        "Setup command failed (exit code {}): {cmd}\n{}",
                        result.exit_code, result.stderr,
                    )));
                }
                self.services
                    .emitter
                    .emit(&WorkflowRunEvent::SetupCommandCompleted {
                        command: cmd.clone(),
                        index,
                        exit_code: result.exit_code,
                        duration_ms: cmd_duration,
                    });
            }
            let setup_duration = crate::millis_u64(setup_start.elapsed());
            self.services
                .emitter
                .emit(&WorkflowRunEvent::SetupCompleted {
                    duration_ms: setup_duration,
                });
        }

        // 6. Run devcontainer lifecycle phases
        for (phase, commands) in &lifecycle.devcontainer_phases {
            crate::devcontainer_bridge::run_devcontainer_lifecycle(
                self.services.sandbox.as_ref(),
                &self.services.emitter,
                phase,
                commands,
                lifecycle.setup_command_timeout_ms,
            )
            .await
            .map_err(|e| FabroError::engine(e.to_string()))?;
        }

        Ok(())
    }

    /// EXECUTE: pure graph traversal. No sandbox setup.
    pub async fn execute_graph(
        &self,
        graph: &Graph,
        settings: &RunSettings,
        checkpoint: Option<&Checkpoint>,
    ) -> Result<Outcome> {
        if let Some(cp) = checkpoint {
            self.run_from_checkpoint(graph, settings, cp).await
        } else {
            self.run(graph, settings).await
        }
    }

    /// Fire the `SandboxCleanup` hook and optionally clean up the sandbox.
    ///
    /// Call this after the retro/PR work is done. The hook fires even when
    /// `preserve` is true (observability), but the actual cleanup is skipped.
    pub async fn cleanup_sandbox(
        &self,
        run_id: &str,
        workflow_name: &str,
        preserve: bool,
    ) -> std::result::Result<(), String> {
        // Fire SandboxCleanup hook (non-blocking)
        let hook_ctx = HookContext::new(
            HookEvent::SandboxCleanup,
            run_id.to_string(),
            workflow_name.to_string(),
        );
        let _ = self.run_hooks(&hook_ctx, None).await;

        if !preserve {
            self.services.sandbox.cleanup().await?;
        }
        Ok(())
    }

    /// Run a workflow seeded with an existing context. Returns both the outcome
    /// and the final context so the caller can diff changes.
    pub async fn run_with_context(
        &self,
        graph: &Graph,
        settings: &RunSettings,
        seed_context: Context,
    ) -> Result<(Outcome, Context)> {
        self.run_via_core(graph, settings, None, Some(seed_context))
            .await
    }

    /// Resume from a checkpoint. Restores context, completed nodes, and continues
    /// execution from the node after the checkpoint's `current_node`.
    ///
    /// # Errors
    ///
    /// Returns an error if the checkpoint's current node is not found or execution fails.
    pub async fn run_from_checkpoint(
        &self,
        graph: &Graph,
        settings: &RunSettings,
        checkpoint: &Checkpoint,
    ) -> Result<Outcome> {
        let (outcome, _context) = self
            .run_via_core(graph, settings, Some(checkpoint), None)
            .await?;
        Ok(outcome)
    }

    /// Run the workflow through the fabro-core executor with full lifecycle management.
    async fn run_via_core(
        &self,
        graph: &Graph,
        settings: &RunSettings,
        resume_checkpoint: Option<&Checkpoint>,
        seed_context: Option<Context>,
    ) -> Result<(Outcome, Context)> {
        let graph_arc = std::sync::Arc::new(graph.clone());
        let wf_graph = crate::core_adapter::WorkflowGraph(Arc::clone(&graph_arc));

        // Populate git_state for handlers (parallel, fan_in) when checkpointing is active
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

        // Build a shared EngineServices for the handler
        let shared_services = std::sync::Arc::new(EngineServices {
            registry: Arc::clone(&self.services.registry),
            emitter: Arc::clone(&self.services.emitter),
            sandbox: Arc::clone(&self.services.sandbox),
            git_state: std::sync::RwLock::new(git_state),
            hook_runner: self.services.hook_runner.clone(),
            env: self.services.env.clone(),
            dry_run: self.services.dry_run,
        });

        // Build handler
        let handler = std::sync::Arc::new(crate::core_adapter::WorkflowNodeHandler {
            services: shared_services,
            run_dir: settings.run_dir.clone(),
            graph: Arc::clone(&graph_arc),
        });

        // Build lifecycle
        let settings_arc = std::sync::Arc::new(settings.clone());
        let lifecycle = crate::core_adapter::WorkflowLifecycle::new(
            self.services.emitter.clone(),
            self.services.hook_runner.clone(),
            self.services.sandbox.clone(),
            graph_arc,
            settings.run_dir.clone(),
            settings_arc,
            resume_checkpoint.is_some(),
        );

        // Restore state from checkpoint
        if let Some(cp) = resume_checkpoint {
            lifecycle.restore_circuit_breaker(
                cp.loop_failure_signatures.clone(),
                cp.restart_failure_signatures.clone(),
            );
            // Degrade fidelity on the first resumed node when prior fidelity was Full
            if cp.context_values.get(context::keys::INTERNAL_FIDELITY)
                == Some(&serde_json::json!(context::keys::Fidelity::Full.to_string()))
            {
                lifecycle.set_degrade_fidelity_on_resume(true);
            }
        }

        // Build RunState
        let state = if let Some(cp) = resume_checkpoint {
            // Resume from checkpoint
            let mut s = RunState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string()))?;
            // Restore context values
            for (k, v) in &cp.context_values {
                s.context.set(k.clone(), v.clone());
            }
            s.completed_nodes = cp.completed_nodes.clone();
            s.node_retries = cp.node_retries.clone();
            // Restore node_visits; reconstruct from completed_nodes for old checkpoints
            if cp.node_visits.is_empty() {
                for id in &cp.completed_nodes {
                    *s.node_visits.entry(id.clone()).or_insert(0) += 1;
                }
            } else {
                s.node_visits = cp.node_visits.clone();
            }
            // Restore node outcomes
            for (k, v) in &cp.node_outcomes {
                s.node_outcomes.insert(k.clone(), v.clone());
            }
            // Set stage_index to number of completed nodes
            s.stage_index = cp.completed_nodes.len();
            // Use stored next_node_id if available, otherwise fall back
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
        } else if let Some(seed) = seed_context {
            let s = RunState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string()))?;
            // Populate from seed context
            for (k, v) in seed.snapshot() {
                s.context.set(k, v);
            }
            s
        } else {
            RunState::new(&wf_graph).map_err(|e| FabroError::engine(e.to_string()))?
        };

        // Compute global visit limit
        let graph_max = graph.max_node_visits();
        let max_node_visits = if graph_max > 0 {
            Some(graph_max as usize)
        } else if settings.dry_run {
            Some(10)
        } else {
            None
        };

        // Set up stall watchdog
        let stall_timeout_opt = graph.stall_timeout();
        let stall_token = stall_timeout_opt.map(|_| CancellationToken::new());
        let stall_shutdown =
            if let (Some(stall_timeout), Some(ref token)) = (stall_timeout_opt, &stall_token) {
                let shutdown = CancellationToken::new();
                let emitter = self.services.emitter.clone();
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
                                // Check if there's been recent activity
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

        // Build executor
        let mut builder = ExecutorBuilder::new(
            handler
                as std::sync::Arc<
                    dyn fabro_core::handler::NodeHandler<crate::core_adapter::WorkflowGraph>,
                >,
        )
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

        // Run
        let result = executor.run(&wf_graph, state).await;

        // Shut down stall poller
        if let Some(shutdown) = stall_shutdown {
            shutdown.cancel();
        }

        // Convert result
        match result {
            Ok((core_outcome, final_state)) => {
                let ctx = final_state.context.clone();
                let result = if core_outcome.status == StageStatus::Fail {
                    core_outcome
                } else {
                    let mut out = Outcome::success();
                    out.notes = Some("Pipeline completed".to_string());
                    out
                };
                Ok((result, ctx))
            }
            Err(fabro_core::CoreError::StallTimeout { node_id }) => {
                let stall_timeout = graph.stall_timeout().unwrap_or_default();
                let idle_secs = stall_timeout.as_secs();
                self.services
                    .emitter
                    .emit(&WorkflowRunEvent::StallWatchdogTimeout {
                        node: node_id.clone(),
                        idle_seconds: idle_secs,
                    });
                Err(FabroError::engine(format!(
                    "stall watchdog: node \"{node_id}\" had no activity for {idle_secs}s"
                )))
            }
            Err(fabro_core::CoreError::Cancelled) => Err(FabroError::Cancelled),
            Err(fabro_core::CoreError::Blocked { message }) => Err(FabroError::engine(message)),
            Err(e) => Err(FabroError::engine(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::start::StartHandler;
    use crate::handler::Handler as HandlerTrait;
    use async_trait::async_trait;
    use fabro_graphviz::graph::AttrValue;
    use std::time::Duration;

    fn local_env() -> Arc<dyn Sandbox> {
        Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ))
    }

    // --- Test-only handlers ---

    /// Handler that always returns Fail.
    struct AlwaysFailHandler;

    #[async_trait]
    impl HandlerTrait for AlwaysFailHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &crate::handler::EngineServices,
        ) -> std::result::Result<Outcome, FabroError> {
            Ok(Outcome::fail_classify("always fails"))
        }
    }

    /// Handler that sleeps for a configurable duration, then succeeds.
    struct SlowHandler {
        sleep_ms: u64,
    }

    #[async_trait]
    impl HandlerTrait for SlowHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &crate::handler::EngineServices,
        ) -> std::result::Result<Outcome, FabroError> {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            Ok(Outcome::success())
        }
    }

    // --- RetryPolicy preset tests ---

    #[test]
    fn retry_policy_none() {
        let policy = RetryPolicy::none();
        assert_eq!(policy.max_attempts, 1);
    }

    #[test]
    fn retry_policy_standard() {
        let policy = RetryPolicy::standard();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(5_000));
    }

    #[test]
    fn retry_policy_aggressive() {
        let policy = RetryPolicy::aggressive();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(500));
    }

    #[test]
    fn retry_policy_linear() {
        let policy = RetryPolicy::linear();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff.factor, 1.0);
    }

    #[test]
    fn visit_from_context_defaults_to_first_visit() {
        let ctx = Context::new();
        assert_eq!(visit_from_context(&ctx), 1);
    }

    #[test]
    fn visit_from_context_preserves_stored_visit() {
        let ctx = Context::new();
        ctx.set(
            crate::context::keys::INTERNAL_NODE_VISIT_COUNT,
            serde_json::json!(3),
        );
        assert_eq!(visit_from_context(&ctx), 3);
    }

    #[test]
    fn retry_policy_patient() {
        let policy = RetryPolicy::patient();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(2000));
    }

    // --- build_retry_policy tests ---

    #[test]
    fn build_retry_policy_from_node() {
        let mut node = Node::new("n");
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 4); // 3 retries + 1 initial
    }

    #[test]
    fn build_retry_policy_from_graph_default() {
        let node = Node::new("n");
        let mut graph = Graph::new("test");
        graph
            .attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(2));
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 3); // 2 retries + 1 initial
    }

    #[test]
    fn build_retry_policy_no_attrs_uses_graph_default_0() {
        let node = Node::new("n");
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 1); // default_max_retries=0 + 1
    }

    #[test]
    fn build_retry_policy_from_retry_policy_attr() {
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String("aggressive".to_string()),
        );
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(500));
    }

    #[test]
    fn build_retry_policy_fallback_when_no_retry_policy_attr() {
        let mut node = Node::new("n");
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 4); // 3 retries + 1 initial
                                            // Should use default backoff, not a preset's backoff
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(5_000));
    }

    #[test]
    fn build_retry_policy_all_presets() {
        let presets = [
            ("none", 1u32),
            ("standard", 5),
            ("aggressive", 5),
            ("linear", 3),
            ("patient", 3),
        ];
        let graph = Graph::new("test");
        let (name, expected) = presets[0];
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[1];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[2];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[3];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[4];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);
    }

    #[test]
    fn build_retry_policy_unknown_preset_falls_back() {
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String("unknown_preset".to_string()),
        );
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        // Unknown preset should fall back to graph default_max_retries=0
        assert_eq!(policy.max_attempts, 1);
    }

    // --- normalize_label tests ---

    #[test]
    fn normalize_label_lowercase_and_trim() {
        assert_eq!(normalize_label("  Yes  "), "yes");
    }

    #[test]
    fn normalize_label_strip_bracket_prefix() {
        assert_eq!(normalize_label("[A] Approve"), "approve");
        assert_eq!(normalize_label("[F] Fix"), "fix");
    }

    #[test]
    fn normalize_label_strip_paren_prefix() {
        assert_eq!(normalize_label("Y) Yes"), "yes");
    }

    #[test]
    fn normalize_label_strip_dash_prefix() {
        assert_eq!(normalize_label("Y - Yes"), "yes");
    }

    #[test]
    fn normalize_label_plain() {
        assert_eq!(normalize_label("next"), "next");
    }

    // --- best_by_weight_then_lexical tests ---

    #[test]
    fn best_by_weight_highest_wins() {
        let e1 = Edge::new("a", "x");
        let mut e2 = Edge::new("a", "y");
        e2.attrs.insert("weight".to_string(), AttrValue::Integer(5));
        let result = best_by_weight_then_lexical(&[&e1, &e2]).unwrap();
        assert_eq!(result.to, "y");
    }

    #[test]
    fn best_by_weight_lexical_tiebreak() {
        let e1 = Edge::new("a", "beta");
        let e2 = Edge::new("a", "alpha");
        let result = best_by_weight_then_lexical(&[&e1, &e2]).unwrap();
        assert_eq!(result.to, "alpha");
    }

    #[test]
    fn best_by_weight_empty_returns_none() {
        let result = best_by_weight_then_lexical(&[]);
        assert!(result.is_none());
    }

    // --- weighted_random tests ---

    #[test]
    fn weighted_random_empty_returns_none() {
        assert!(weighted_random(&[]).is_none());
    }

    #[test]
    fn weighted_random_single_edge() {
        let e = Edge::new("a", "b");
        let result = weighted_random(&[&e]).unwrap();
        assert_eq!(result.to, "b");
    }

    #[test]
    fn weighted_random_zero_weight_all_selected() {
        let e1 = Edge::new("a", "b");
        let e2 = Edge::new("a", "c");
        let edges = vec![&e1, &e2];
        let mut seen_b = false;
        let mut seen_c = false;
        for _ in 0..200 {
            let pick = weighted_random(&edges).unwrap();
            if pick.to == "b" {
                seen_b = true;
            }
            if pick.to == "c" {
                seen_c = true;
            }
        }
        assert!(seen_b, "expected target 'b' to be selected at least once");
        assert!(seen_c, "expected target 'c' to be selected at least once");
    }

    #[test]
    fn weighted_random_high_weight_dominates() {
        let mut heavy = Edge::new("a", "heavy");
        heavy
            .attrs
            .insert("weight".to_string(), AttrValue::Integer(100));
        let mut light = Edge::new("a", "light");
        light
            .attrs
            .insert("weight".to_string(), AttrValue::Integer(1));
        let edges = vec![&heavy, &light];
        let mut heavy_count = 0;
        for _ in 0..500 {
            let pick = weighted_random(&edges).unwrap();
            if pick.to == "heavy" {
                heavy_count += 1;
            }
        }
        let ratio = heavy_count as f64 / 500.0;
        assert!(
            ratio > 0.90,
            "expected heavy edge to win >90% of the time, got {ratio:.2}"
        );
    }

    // --- select_edge tests ---

    fn make_graph_with_edges(edges: Vec<Edge>) -> Graph {
        let mut g = Graph::new("test");
        for edge in &edges {
            if !g.nodes.contains_key(&edge.from) {
                g.nodes.insert(edge.from.clone(), Node::new(&edge.from));
            }
            if !g.nodes.contains_key(&edge.to) {
                g.nodes.insert(edge.to.clone(), Node::new(&edge.to));
            }
        }
        g.edges = edges;
        g
    }

    #[test]
    fn select_edge_no_edges() {
        let g = Graph::new("test");
        let node = Node::new("a");
        let outcome = Outcome::success();
        let context = Context::new();
        assert!(select_edge(&node, &outcome, &context, &g, "deterministic").is_none());
    }

    #[test]
    fn select_edge_single_unconditional() {
        let g = make_graph_with_edges(vec![Edge::new("a", "b")]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "b");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_condition_match() {
        let mut e1 = Edge::new("a", "fail_path");
        e1.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let mut e2 = Edge::new("a", "success_path");
        e2.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "success_path");
        assert_eq!(sel.reason, "condition");
    }

    #[test]
    fn select_edge_preferred_label() {
        let mut e1 = Edge::new("a", "approve");
        e1.attrs.insert(
            "label".to_string(),
            AttrValue::String("[A] Approve".to_string()),
        );
        let mut e2 = Edge::new("a", "fix");
        e2.attrs.insert(
            "label".to_string(),
            AttrValue::String("[F] Fix".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let mut outcome = Outcome::success();
        outcome.preferred_label = Some("Fix".to_string());
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "fix");
        assert_eq!(sel.reason, "preferred_label");
    }

    #[test]
    fn select_edge_suggested_next_ids() {
        let e1 = Edge::new("a", "path1");
        let e2 = Edge::new("a", "path2");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let mut outcome = Outcome::success();
        outcome.suggested_next_ids = vec!["path2".to_string()];
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "path2");
        assert_eq!(sel.reason, "suggested_next");
    }

    #[test]
    fn select_edge_weight_tiebreak() {
        let mut e1 = Edge::new("a", "low");
        e1.attrs.insert("weight".to_string(), AttrValue::Integer(1));
        let mut e2 = Edge::new("a", "high");
        e2.attrs
            .insert("weight".to_string(), AttrValue::Integer(10));
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "high");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_lexical_tiebreak() {
        let e1 = Edge::new("a", "charlie");
        let e2 = Edge::new("a", "alpha");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "alpha");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_condition_beats_unconditional() {
        let mut e_cond = Edge::new("a", "cond_path");
        e_cond.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        let e_uncond = Edge::new("a", "uncond_path");
        let g = make_graph_with_edges(vec![e_cond, e_uncond]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "cond_path");
        assert_eq!(sel.reason, "condition");
    }

    #[test]
    fn select_edge_random_returns_some_edge() {
        let e1 = Edge::new("a", "b");
        let e2 = Edge::new("a", "c");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "random").unwrap();
        assert!(sel.edge.to == "b" || sel.edge.to == "c");
        assert_eq!(sel.reason, "unconditional");
    }

    #[test]
    fn select_edge_random_preferred_label_still_wins() {
        let mut e1 = Edge::new("a", "approve");
        e1.attrs.insert(
            "label".to_string(),
            AttrValue::String("Approve".to_string()),
        );
        let e2 = Edge::new("a", "other");
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let mut outcome = Outcome::success();
        outcome.preferred_label = Some("Approve".to_string());
        let context = Context::new();
        let sel = select_edge(node, &outcome, &context, &g, "random").unwrap();
        assert_eq!(sel.edge.to, "approve");
        assert_eq!(sel.reason, "preferred_label");
    }

    #[test]
    fn select_edge_failed_human_gate_does_not_fall_through_to_unconditional() {
        let g = make_graph_with_edges(vec![
            Edge::new("gate", "approve"),
            Edge::new("gate", "skip"),
        ]);
        let mut node = g.nodes.get("gate").unwrap().clone();
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        let outcome =
            Outcome::fail_deterministic("human interaction aborted before an answer was provided");
        let context = Context::new();

        assert!(select_edge(&node, &outcome, &context, &g, "deterministic").is_none());
    }

    #[test]
    fn select_edge_failed_human_gate_routes_via_fail_condition() {
        let mut fail = Edge::new("gate", "retry");
        fail.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let approve = Edge::new("gate", "approve");
        let g = make_graph_with_edges(vec![fail, approve]);
        let mut node = g.nodes.get("gate").unwrap().clone();
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        let outcome =
            Outcome::fail_deterministic("human interaction aborted before an answer was provided");
        let context = Context::new();

        let sel = select_edge(&node, &outcome, &context, &g, "deterministic").unwrap();
        assert_eq!(sel.edge.to, "retry");
        assert_eq!(sel.reason, "condition");
    }

    #[test]
    fn select_edge_deterministic_no_fallback_when_no_condition_matches() {
        let mut e1 = Edge::new("a", "path1");
        e1.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let mut e2 = Edge::new("a", "path2");
        e2.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=error".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        assert!(select_edge(node, &outcome, &context, &g, "deterministic").is_none());
    }

    #[test]
    fn select_edge_random_no_fallback_when_no_condition_matches() {
        let mut e1 = Edge::new("a", "path1");
        e1.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        let mut e2 = Edge::new("a", "path2");
        e2.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=error".to_string()),
        );
        let g = make_graph_with_edges(vec![e1, e2]);
        let node = g.nodes.get("a").unwrap();
        let outcome = Outcome::success();
        let context = Context::new();
        assert!(select_edge(node, &outcome, &context, &g, "random").is_none());
    }

    // --- check_goal_gates tests ---

    #[test]
    fn goal_gates_all_satisfied() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), n);

        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::success());

        assert!(check_goal_gates(&g, &outcomes).is_ok());
    }

    #[test]
    fn goal_gates_partial_success_counts() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), n);

        let mut outcomes = HashMap::new();
        let mut o = Outcome::success();
        o.status = StageStatus::PartialSuccess;
        outcomes.insert("work".to_string(), o);

        assert!(check_goal_gates(&g, &outcomes).is_ok());
    }

    #[test]
    fn goal_gates_failed_returns_node_id() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs
            .insert("goal_gate".to_string(), AttrValue::Boolean(true));
        g.nodes.insert("work".to_string(), n);

        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::fail_classify("test"));

        assert_eq!(check_goal_gates(&g, &outcomes), Err("work".to_string()));
    }

    #[test]
    fn goal_gates_non_gate_nodes_ignored() {
        let mut g = Graph::new("test");
        g.nodes.insert("work".to_string(), Node::new("work"));

        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::fail_classify("test"));

        assert!(check_goal_gates(&g, &outcomes).is_ok());
    }

    // --- get_retry_target tests ---

    #[test]
    fn retry_target_from_node() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("plan".to_string()),
        );
        g.nodes.insert("work".to_string(), n);
        g.nodes.insert("plan".to_string(), Node::new("plan"));

        assert_eq!(get_retry_target("work", &g), Some("plan".to_string()));
    }

    #[test]
    fn retry_target_from_fallback() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs.insert(
            "fallback_retry_target".to_string(),
            AttrValue::String("plan".to_string()),
        );
        g.nodes.insert("work".to_string(), n);
        g.nodes.insert("plan".to_string(), Node::new("plan"));

        assert_eq!(get_retry_target("work", &g), Some("plan".to_string()));
    }

    #[test]
    fn retry_target_from_graph() {
        let mut g = Graph::new("test");
        g.nodes.insert("work".to_string(), Node::new("work"));
        g.nodes.insert("plan".to_string(), Node::new("plan"));
        g.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("plan".to_string()),
        );

        assert_eq!(get_retry_target("work", &g), Some("plan".to_string()));
    }

    #[test]
    fn retry_target_none_when_missing() {
        let mut g = Graph::new("test");
        g.nodes.insert("work".to_string(), Node::new("work"));
        assert!(get_retry_target("work", &g).is_none());
    }

    #[test]
    fn retry_target_skips_nonexistent_node() {
        let mut g = Graph::new("test");
        let mut n = Node::new("work");
        n.attrs.insert(
            "retry_target".to_string(),
            AttrValue::String("nonexistent".to_string()),
        );
        g.nodes.insert("work".to_string(), n);
        // No "nonexistent" node -- should fall through to graph-level
        assert!(get_retry_target("work", &g).is_none());
    }

    // --- is_terminal tests ---

    #[test]
    fn terminal_by_shape() {
        let mut n = Node::new("exit");
        n.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        assert!(is_terminal(&n));
    }

    #[test]
    fn terminal_by_type() {
        let mut n = Node::new("end");
        n.attrs
            .insert("type".to_string(), AttrValue::String("exit".to_string()));
        assert!(is_terminal(&n));
    }

    #[test]
    fn non_terminal_node() {
        let n = Node::new("work");
        assert!(!is_terminal(&n));
    }

    // --- WorkflowRunEngine integration tests ---

    fn simple_graph() -> Graph {
        let mut g = Graph::new("test_pipeline");
        g.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Run tests".to_string()),
        );

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "exit"));
        g
    }

    fn make_registry() -> HandlerRegistry {
        use crate::handler::exit::ExitHandler;
        let mut registry = HandlerRegistry::new(Box::new(StartHandler));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry
    }

    #[tokio::test]
    async fn engine_runs_simple_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: Some(GitCheckpointSettings {
                base_sha: None,
                run_branch: Some("fabro/run/test-run".into()),
                meta_branch: None,
            }),
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn engine_saves_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: Some(GitCheckpointSettings {
                base_sha: None,
                run_branch: Some("fabro/run/test-run".into()),
                meta_branch: None,
            }),
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();
        let checkpoint_path = dir.path().join("checkpoint.json");
        assert!(checkpoint_path.exists());
    }

    #[tokio::test]
    async fn engine_emits_events() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let emitter = EventEmitter::new();
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(format!("{event:?}"));
        });

        let engine = WorkflowRunEngine::new(make_registry(), Arc::new(emitter), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: Some(GitCheckpointSettings {
                base_sha: None,
                run_branch: Some("fabro/run/test-run".into()),
                meta_branch: None,
            }),
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let collected = events.lock().unwrap();
        // Should have: RunStarted, StageStarted (start), StageCompleted (start),
        // CheckpointCompleted, RunCompleted
        assert!(collected.len() >= 4);
    }

    #[tokio::test]
    async fn engine_error_when_no_start_node() {
        let dir = tempfile::tempdir().unwrap();
        let g = Graph::new("empty");
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: Some(GitCheckpointSettings {
                base_sha: None,
                run_branch: Some("fabro/run/test-run".into()),
                meta_branch: None,
            }),
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn engine_mirrors_graph_goal_to_context() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        // Verify checkpoint has graph.goal mirrored
        let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        assert_eq!(
            cp.context_values.get(context::keys::GRAPH_GOAL),
            Some(&serde_json::json!("Run tests"))
        );
    }

    #[tokio::test]
    async fn engine_multi_node_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = simple_graph();
        // Insert a work node between start and exit
        let work = Node::new("work");
        g.nodes.insert("work".to_string(), work);
        g.edges.clear();
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);

        // Checkpoint should show work was completed
        let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        assert!(cp.completed_nodes.contains(&"start".to_string()));
        assert!(cp.completed_nodes.contains(&"work".to_string()));
    }

    #[tokio::test]
    async fn engine_conditional_routing() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("cond_test");

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.nodes.insert("path_a".to_string(), Node::new("path_a"));
        g.nodes.insert("path_b".to_string(), Node::new("path_b"));

        // start -> path_a (condition: outcome=fail)
        let mut e1 = Edge::new("start", "path_a");
        e1.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        g.edges.push(e1);

        // start -> path_b (unconditional, should be taken since start returns success)
        g.edges.push(Edge::new("start", "path_b"));

        g.edges.push(Edge::new("path_a", "exit"));
        g.edges.push(Edge::new("path_b", "exit"));

        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        // Should have gone through path_b (unconditional) not path_a (condition=fail)
        assert!(cp.completed_nodes.contains(&"path_b".to_string()));
        assert!(!cp.completed_nodes.contains(&"path_a".to_string()));
    }

    // --- resolve_fidelity tests ---

    #[test]
    fn fidelity_defaults_to_compact() {
        use crate::context::keys::Fidelity;
        let node = Node::new("work");
        let graph = Graph::new("test");
        assert_eq!(resolve_fidelity(None, &node, &graph), Fidelity::Compact);
    }

    #[test]
    fn fidelity_from_graph_default() {
        use crate::context::keys::Fidelity;
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
        use crate::context::keys::Fidelity;
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
        use crate::context::keys::Fidelity;
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

    // --- start.json and node status tests ---

    #[tokio::test]
    async fn engine_writes_start_json() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: Some(GitCheckpointSettings {
                base_sha: None,
                run_branch: Some("fabro/run/test-run".into()),
                meta_branch: None,
            }),
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let start = crate::start_record::StartRecord::load(dir.path()).unwrap();
        assert_eq!(start.run_id, "test-run");
        assert_eq!(start.run_branch.as_deref(), Some("fabro/run/test-run"));
    }

    #[tokio::test]
    async fn start_record_includes_base_sha() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "sha-run".into(),
            config: FabroConfig::default(),
            git: Some(GitCheckpointSettings {
                base_sha: Some("abc123".into()),
                run_branch: None,
                meta_branch: None,
            }),
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let start = crate::start_record::StartRecord::load(dir.path()).unwrap();
        assert_eq!(start.base_sha.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn start_record_omits_optional_fields_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "no-optional-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let start = crate::start_record::StartRecord::load(dir.path()).unwrap();
        assert!(start.run_branch.is_none());
        assert!(start.base_sha.is_none());
    }

    #[tokio::test]
    async fn engine_writes_node_status_json() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        // start node should have status.json
        let status_path = dir.path().join("nodes").join("start").join("status.json");
        assert!(status_path.exists());
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(status["status"], "success");
    }

    #[tokio::test]
    async fn engine_stores_fidelity_in_context() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        // The checkpoint context should contain internal.fidelity
        let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        assert_eq!(
            cp.context_values.get(context::keys::INTERNAL_FIDELITY),
            Some(&serde_json::json!("compact"))
        );
    }

    // --- resolve_thread_id tests ---

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
        // When the edge has no thread_id, the node's thread_id is used.
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
        // Edge thread_id should take precedence over node thread_id,
        // matching the fidelity precedence where edge > node.
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

    // --- Gap #15: StartRecord run_id field test ---

    #[tokio::test]
    async fn engine_start_record_has_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let start = crate::start_record::StartRecord::load(dir.path()).unwrap();
        assert_eq!(start.run_id, "test-run");
    }

    #[tokio::test]
    async fn engine_start_record_run_branch_none_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("no_goal");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);
        g.edges.push(Edge::new("start", "exit"));

        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let start = crate::start_record::StartRecord::load(dir.path()).unwrap();
        assert!(start.run_branch.is_none());
    }

    // --- Gap #1: Auto status tests ---

    #[tokio::test]
    async fn engine_auto_status_overrides_fail_to_success() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("auto_status_test");

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs
            .insert("auto_status".to_string(), AttrValue::Boolean(true));
        work.attrs.insert(
            "type".to_string(),
            AttrValue::String("always_fail".to_string()),
        );
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(AlwaysFailHandler));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();

        // Pipeline outcome is always SUCCESS when goal gates are satisfied
        assert_eq!(outcome.status, StageStatus::Success);
        assert_eq!(outcome.notes.as_deref(), Some("Pipeline completed"));

        // The auto_status note is on the per-node status.json
        let status_path = dir.path().join("nodes").join("work").join("status.json");
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(status["status"], "success");
    }

    #[tokio::test]
    async fn engine_auto_status_false_preserves_fail() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("no_auto_status_test");

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "type".to_string(),
            AttrValue::String("always_fail".to_string()),
        );
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        let mut fail_edge = Edge::new("work", "exit");
        fail_edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        g.edges.push(fail_edge);

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(AlwaysFailHandler));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;

        assert!(result.is_ok());
        let status_path = dir.path().join("nodes").join("work").join("status.json");
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(status["status"], "fail");
    }

    // --- Gap #2: Timeout enforcement tests ---

    #[tokio::test]
    async fn engine_timeout_causes_fail_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("timeout_test");

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "timeout".to_string(),
            AttrValue::Duration(Duration::from_millis(50)),
        );
        work.attrs
            .insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        let mut fail_edge = Edge::new("work", "exit");
        fail_edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        g.edges.push(fail_edge);

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 500 }));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_ok());

        let status_path = dir.path().join("nodes").join("work").join("status.json");
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(status["status"], "fail");
    }

    #[tokio::test]
    async fn engine_no_timeout_completes_normally() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("no_timeout_test");

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs
            .insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 10 }));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn engine_timeout_with_auto_status_returns_success() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("timeout_auto_status_test");

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "timeout".to_string(),
            AttrValue::Duration(Duration::from_millis(50)),
        );
        work.attrs
            .insert("auto_status".to_string(), AttrValue::Boolean(true));
        work.attrs
            .insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 500 }));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();

        // Pipeline outcome is always SUCCESS when goal gates are satisfied
        assert_eq!(outcome.status, StageStatus::Success);
        assert_eq!(outcome.notes.as_deref(), Some("Pipeline completed"));

        // The auto_status note is on the per-node status.json
        let status_path = dir.path().join("nodes").join("work").join("status.json");
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(status["status"], "success");
    }

    // --- Gap #15: Interviewer.inform() tests ---

    #[tokio::test]
    async fn engine_without_interviewer_runs_normally() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    // --- Gap #7: Cancellation token tests ---

    #[tokio::test]
    async fn engine_returns_cancelled_when_token_set_before_run() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let cancel_token = Arc::new(AtomicBool::new(true));
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: Some(cancel_token),
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FabroError::Cancelled));
    }

    #[tokio::test]
    async fn engine_runs_normally_with_unset_cancel_token() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let cancel_token = Arc::new(AtomicBool::new(false));
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: Some(cancel_token),
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn engine_cancelled_mid_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = simple_graph();
        // Insert a work node between start and exit
        let mut work = Node::new("work");
        work.attrs
            .insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);
        g.edges.clear();
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancel_token_clone = Arc::clone(&cancel_token);

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 200 }));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: Some(cancel_token),
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };

        // Set cancel after a short delay (while the slow handler is running)
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_token_clone.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        let result = engine.run(&g, &config).await;
        // The engine should detect cancellation at the next loop iteration
        // after the slow handler completes
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FabroError::Cancelled));
    }

    // --- max_node_visits tests ---

    /// Build a graph with a cycle: start -> work -> work (unconditional self-loop)
    fn cyclic_graph() -> Graph {
        let mut g = Graph::new("cyclic");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("loop".to_string()));
        // Disable default retries to keep test fast
        g.attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(0));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let work = Node::new("work");
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        // start -> work -> work (self-loop), work -> exit (conditional, never matches)
        g.edges.push(Edge::new("start", "work"));
        let mut cond_edge = Edge::new("work", "exit");
        cond_edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=never_matches".to_string()),
        );
        g.edges.push(cond_edge);
        g.edges.push(Edge::new("work", "work"));
        g
    }

    #[tokio::test]
    async fn max_node_visits_errors_on_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = cyclic_graph();
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(3));
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stuck in a cycle"),
            "expected visit limit error, got: {err}"
        );
    }

    #[tokio::test]
    async fn dry_run_applies_default_visit_limit() {
        let dir = tempfile::tempdir().unwrap();
        let g = cyclic_graph();
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: true,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stuck in a cycle"),
            "expected visit limit error, got: {err}"
        );
    }

    #[tokio::test]
    async fn graph_attr_overrides_dry_run_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = cyclic_graph();
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(2));
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: true,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("(graph limit 2)"),
            "expected graph limit of 2, got: {err}"
        );
    }

    #[tokio::test]
    async fn per_node_max_visits_fires_before_graph_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = cyclic_graph();
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(100));
        g.nodes
            .get_mut("work")
            .unwrap()
            .attrs
            .insert("max_visits".to_string(), AttrValue::Integer(2));
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("node limit 2"),
            "expected node limit 2, got: {err}"
        );
    }

    #[tokio::test]
    async fn per_node_max_visits_overrides_dry_run_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = cyclic_graph();
        g.nodes
            .get_mut("work")
            .unwrap()
            .attrs
            .insert("max_visits".to_string(), AttrValue::Integer(3));
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: true,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("node limit 3"),
            "expected node limit 3, got: {err}"
        );
    }

    #[tokio::test]
    async fn graph_limit_works_without_per_node_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = cyclic_graph();
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(3));
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("graph limit 3"),
            "expected graph limit 3, got: {err}"
        );
    }

    // --- node_dir visit-count tests ---

    #[test]
    fn node_dir_first_visit() {
        let root = Path::new("/tmp/logs");
        assert_eq!(node_dir(root, "work", 1), root.join("nodes").join("work"));
    }

    #[test]
    fn node_dir_second_visit() {
        let root = Path::new("/tmp/logs");
        assert_eq!(
            node_dir(root, "work", 2),
            root.join("nodes").join("work-visit_2")
        );
    }

    #[test]
    fn node_dir_fifth_visit() {
        let root = Path::new("/tmp/logs");
        assert_eq!(
            node_dir(root, "work", 5),
            root.join("nodes").join("work-visit_5")
        );
    }

    // --- panic.txt tests ---

    /// Handler that always panics.
    struct PanickingHandler;

    #[async_trait]
    impl HandlerTrait for PanickingHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &crate::handler::EngineServices,
        ) -> std::result::Result<Outcome, FabroError> {
            panic!("test panic message");
        }
    }

    #[tokio::test]
    async fn panic_handler_writes_panic_txt() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let mut panic_node = Node::new("boom");
        panic_node.attrs.insert(
            "type".to_string(),
            AttrValue::String("panicker".to_string()),
        );
        panic_node
            .attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("boom".to_string(), panic_node);
        g.edges.push(Edge::new("start", "boom"));

        let mut registry = make_registry();
        registry.register("panicker", Box::new(PanickingHandler));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };

        // The engine returns a Fail outcome because there is no outgoing fail edge,
        // but panic.txt should already be written by the panic handler.
        let _result = engine.run(&g, &config).await;

        let panic_path = dir.path().join("nodes").join("boom").join("panic.txt");
        assert!(panic_path.exists(), "panic.txt should be written");
        let content = std::fs::read_to_string(&panic_path).unwrap();
        assert!(
            content.contains("test panic message"),
            "panic.txt should contain the panic message, got: {content}"
        );
    }

    // --- classify_outcome tests ---

    #[test]
    fn classify_outcome_returns_none_for_success() {
        assert!(classify_outcome(&Outcome::success()).is_none());
    }

    #[test]
    fn classify_outcome_returns_none_for_skipped() {
        assert!(classify_outcome(&Outcome::skipped("")).is_none());
    }

    #[test]
    fn classify_outcome_returns_none_for_partial_success() {
        let outcome = Outcome {
            status: StageStatus::PartialSuccess,
            ..Outcome::success()
        };
        assert!(classify_outcome(&outcome).is_none());
    }

    #[test]
    fn classify_outcome_reads_failure_detail() {
        let mut outcome = Outcome::fail_classify("some error");
        // Override the FailureDetail's class directly
        outcome.failure.as_mut().unwrap().category = FailureCategory::BudgetExhausted;
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::BudgetExhausted)
        );
    }

    #[test]
    fn classify_outcome_uses_failure_reason_heuristics() {
        let outcome = Outcome::fail_classify("rate limited by provider");
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::TransientInfra)
        );
    }

    #[test]
    fn classify_outcome_defaults_to_deterministic() {
        let outcome = Outcome::fail_classify("something went wrong");
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::Deterministic)
        );
    }

    #[test]
    fn classify_outcome_fail_no_reason_is_deterministic() {
        let outcome = Outcome {
            status: StageStatus::Fail,
            failure: None,
            ..Outcome::success()
        };
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::Deterministic)
        );
    }

    #[test]
    fn classify_outcome_retry_status_uses_heuristics() {
        let outcome = Outcome::retry_classify("connection refused");
        assert_eq!(
            classify_outcome(&outcome),
            Some(FailureCategory::TransientInfra)
        );
    }

    // --- Circuit breaker tests ---

    /// Build a graph where `work` always fails deterministically,
    /// and a fail edge loops back to `work`.
    fn looping_fail_graph() -> Graph {
        let mut g = Graph::new("loop_fail");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("test".to_string()));
        g.attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(0));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "type".to_string(),
            AttrValue::String("always_fail".to_string()),
        );
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        // Fail loops back
        let mut fail_edge = Edge::new("work", "work");
        fail_edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        g.edges.push(fail_edge);
        // Success goes to exit (never taken)
        let mut ok_edge = Edge::new("work", "exit");
        ok_edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        g.edges.push(ok_edge);
        g
    }

    /// Handler that always returns transient_infra failure.
    struct TransientFailHandler;

    #[async_trait]
    impl HandlerTrait for TransientFailHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &crate::handler::EngineServices,
        ) -> std::result::Result<Outcome, FabroError> {
            Ok(Outcome::fail_classify("connection refused"))
        }
    }

    /// Handler that fails with a semantically different message each time.
    /// Uses words instead of numbers to avoid normalization collapsing them.
    struct VaryingFailHandler {
        counter: std::sync::atomic::AtomicUsize,
    }

    static VARYING_REASONS: &[&str] = &[
        "syntax error in module alpha",
        "type mismatch in module beta",
        "missing field in module gamma",
        "undefined reference in module delta",
        "assertion failed in module epsilon",
    ];

    #[async_trait]
    impl HandlerTrait for VaryingFailHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &crate::handler::EngineServices,
        ) -> std::result::Result<Outcome, FabroError> {
            let n = self
                .counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let reason = VARYING_REASONS[n % VARYING_REASONS.len()];
            Ok(Outcome::fail_classify(reason))
        }
    }

    #[tokio::test]
    async fn loop_circuit_breaker_aborts_on_repeated_deterministic_failure() {
        let dir = tempfile::tempdir().unwrap();
        let g = looping_fail_graph();

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(AlwaysFailHandler));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("deterministic failure cycle detected"),
            "expected circuit breaker error, got: {err}"
        );
    }

    #[tokio::test]
    async fn loop_circuit_breaker_ignores_transient_failures() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = looping_fail_graph();
        // Set a high visit limit so we don't trip it; we want to hit the visit limit, not circuit breaker
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(5));

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(TransientFailHandler));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // Should hit visit limit, NOT circuit breaker
        assert!(
            err.contains("stuck in a cycle"),
            "expected visit limit error (transient shouldn't trigger circuit breaker), got: {err}"
        );
    }

    #[tokio::test]
    async fn loop_circuit_breaker_different_reasons_get_separate_counters() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = looping_fail_graph();
        // Each failure has a different message, so no signature repeats.
        // Should hit max_node_visits instead of circuit breaker.
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(5));

        let mut registry = make_registry();
        registry.register(
            "always_fail",
            Box::new(VaryingFailHandler {
                counter: std::sync::atomic::AtomicUsize::new(0),
            }),
        );
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stuck in a cycle"),
            "expected visit limit (each failure unique), got: {err}"
        );
    }

    #[tokio::test]
    async fn restart_circuit_breaker_aborts_on_repeated_failure() {
        // In a workflow with loop_restart edges, a repeating deterministic failure
        // triggers a circuit breaker (either loop or restart, depending on topology).
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("restart_test");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("test".to_string()));
        g.attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(0));
        g.attrs
            .insert("max_node_visits".to_string(), AttrValue::Integer(100));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "type".to_string(),
            AttrValue::String("always_fail".to_string()),
        );
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        // loop_restart edge on failure
        let mut restart_edge = Edge::new("work", "start");
        restart_edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=fail".to_string()),
        );
        restart_edge
            .attrs
            .insert("loop_restart".to_string(), AttrValue::Boolean(true));
        g.edges.push(restart_edge);
        // Success goes to exit
        let mut ok_edge = Edge::new("work", "exit");
        ok_edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        g.edges.push(ok_edge);

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(AlwaysFailHandler));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // The loop_restart guard blocks non-transient_infra failures immediately
        assert!(
            err.contains("loop_restart blocked")
                || err.contains("failure cycle detected")
                || err.contains("circuit breaker"),
            "expected loop_restart guard or circuit breaker error, got: {err}"
        );
    }

    /// Handler that emits events every `interval_ms` for `total_ms`, then succeeds.
    struct EmittingHandler {
        interval_ms: u64,
        total_ms: u64,
    }

    #[async_trait]
    impl HandlerTrait for EmittingHandler {
        async fn execute(
            &self,
            node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            services: &crate::handler::EngineServices,
        ) -> std::result::Result<Outcome, FabroError> {
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(self.total_ms) {
                tokio::time::sleep(Duration::from_millis(self.interval_ms)).await;
                services.emitter.emit(&WorkflowRunEvent::Prompt {
                    stage: node.id.clone(),
                    text: "keepalive".to_string(),
                });
            }
            Ok(Outcome::success())
        }
    }

    #[tokio::test]
    async fn stall_watchdog_triggers_on_hung_handler() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("stall_test");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("test".to_string()));
        g.attrs.insert(
            "stall_timeout".to_string(),
            AttrValue::Duration(Duration::from_millis(50)),
        );
        g.attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(0));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs
            .insert("type".to_string(), AttrValue::String("slow".to_string()));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 60_000 }));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stall watchdog"),
            "expected stall watchdog error, got: {err}"
        );
    }

    #[tokio::test]
    async fn stall_watchdog_active_handler_resets_timer() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("stall_active_test");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("test".to_string()));
        g.attrs.insert(
            "stall_timeout".to_string(),
            AttrValue::Duration(Duration::from_millis(100)),
        );
        g.attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(0));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "type".to_string(),
            AttrValue::String("emitting".to_string()),
        );
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register(
            "emitting",
            Box::new(EmittingHandler {
                interval_ms: 10,
                total_ms: 50,
            }),
        );
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn stall_watchdog_disabled_when_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("stall_disabled_test");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("test".to_string()));
        g.attrs.insert(
            "stall_timeout".to_string(),
            AttrValue::Duration(Duration::ZERO),
        );
        g.attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(0));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs
            .insert("type".to_string(), AttrValue::String("slow".to_string()));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 50 }));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn failure_signature_stored_in_context() {
        let dir = tempfile::tempdir().unwrap();
        // Simple workflow: start -> work (fails) -> exit (via fail edge)
        let mut g = Graph::new("sig_context_test");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("test".to_string()));
        g.attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(0));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "type".to_string(),
            AttrValue::String("always_fail".to_string()),
        );
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(AlwaysFailHandler));
        let engine = WorkflowRunEngine::new(registry, Arc::new(EventEmitter::new()), local_env());
        let config = RunSettings {
            run_dir: dir.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "test-run".into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        let _outcome = engine.run(&g, &config).await.unwrap();

        // Check the checkpoint for the failure_signature context value
        let checkpoint_path = dir.path().join("checkpoint.json");
        let cp = Checkpoint::load(&checkpoint_path).unwrap();
        let sig_value = cp
            .context_values
            .get(context::keys::FAILURE_SIGNATURE)
            .unwrap();
        let sig_str = sig_value.as_str().unwrap();
        assert!(
            sig_str.contains("work|deterministic|"),
            "expected failure signature in context, got: {sig_str}"
        );
    }

    #[tokio::test]
    async fn git_checkpoint_includes_builtin_excludes() {
        // Set up a real git repo
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .current_dir(repo)
            .output()
            .unwrap();

        // Create files in both tracked and excluded directories
        std::fs::write(repo.join("hello.txt"), "hello").unwrap();
        std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
        std::fs::write(repo.join("node_modules/pkg/index.js"), "module").unwrap();
        std::fs::create_dir_all(repo.join(".venv/lib")).unwrap();
        std::fs::write(repo.join(".venv/lib/site.py"), "venv").unwrap();

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let author = crate::git::GitAuthor::default();

        // Call git_checkpoint with empty user excludes — built-in excludes should still apply
        let result =
            git_checkpoint(&sandbox, "run1", "work", "success", 1, None, &[], &author).await;
        assert!(result.is_ok(), "git_checkpoint failed: {:?}", result.err());

        // Verify that excluded directories were NOT staged
        let status = sandbox
            .exec_command(
                "git diff --cached --name-only HEAD~1",
                10_000,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let staged_files: Vec<&str> = status.stdout.lines().collect();
        assert!(
            staged_files.contains(&"hello.txt"),
            "expected hello.txt to be staged, got: {staged_files:?}"
        );
        assert!(
            !staged_files.iter().any(|f| f.contains("node_modules")),
            "node_modules should be excluded from checkpoint, got: {staged_files:?}"
        );
        assert!(
            !staged_files.iter().any(|f| f.contains(".venv")),
            ".venv should be excluded from checkpoint, got: {staged_files:?}"
        );
    }

    #[tokio::test]
    async fn git_checkpoint_skipped_for_start_node() {
        // Set up a real git repo for checkpoint testing
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .current_dir(repo)
            .output()
            .unwrap();
        let base_sha = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let run_tmp = tempfile::tempdir().unwrap();

        // Build start -> work -> exit graph so work node produces a git checkpoint
        let mut g = simple_graph();
        let work = Node::new("work");
        g.nodes.insert("work".to_string(), work);
        g.edges.clear();
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<WorkflowRunEvent>::new()));
        let events_clone = events.clone();
        let emitter = EventEmitter::new();
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });

        // Use a LocalSandbox pointing at the repo so sandbox.exec_command() runs git there
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(fabro_agent::LocalSandbox::new(repo.to_path_buf()));
        let engine = WorkflowRunEngine::new(make_registry(), Arc::new(emitter), sandbox);
        let config = RunSettings {
            run_dir: run_tmp.path().to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: "git-cp-test".into(),
            config: FabroConfig::default(),
            git: Some(GitCheckpointSettings {
                base_sha: Some(base_sha),
                run_branch: None,
                meta_branch: Some(crate::git::MetadataStore::branch_name("git-cp-test")),
            }),
            host_repo_path: Some(repo.to_path_buf()),
            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        };
        engine.run(&g, &config).await.unwrap();

        let collected = events.lock().unwrap();
        let git_checkpoint_node_ids: Vec<&str> = collected
            .iter()
            .filter_map(|e| match e {
                WorkflowRunEvent::CheckpointCompleted {
                    node_id,
                    git_commit_sha: Some(_),
                    ..
                } => Some(node_id.as_str()),
                _ => None,
            })
            .collect();

        assert!(
            !git_checkpoint_node_ids.contains(&"start"),
            "start node should not have a git checkpoint, but found: {git_checkpoint_node_ids:?}"
        );
        assert!(
            git_checkpoint_node_ids.contains(&"work"),
            "work node should have a git checkpoint, but found: {git_checkpoint_node_ids:?}"
        );
    }

    fn test_run_settings(run_dir: &std::path::Path, run_id: &str) -> RunSettings {
        RunSettings {
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            dry_run: false,
            run_id: run_id.into(),
            config: FabroConfig::default(),
            git: None,
            host_repo_path: None,

            labels: HashMap::new(),
            github_app: None,
            git_author: crate::git::GitAuthor::default(),
            base_branch: None,
            workflow_slug: None,
        }
    }

    fn test_lifecycle(setup_commands: Vec<String>) -> LifecycleConfig {
        LifecycleConfig {
            setup_commands,
            setup_command_timeout_ms: 300_000,
            devcontainer_phases: Vec::new(),
        }
    }

    #[tokio::test]
    async fn run_with_lifecycle_fires_sandbox_initialized_event() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();

        let events = Arc::new(std::sync::Mutex::new(Vec::<WorkflowRunEvent>::new()));
        let events_clone = events.clone();
        let emitter = EventEmitter::new();
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });

        let engine = WorkflowRunEngine::new(make_registry(), Arc::new(emitter), local_env());
        let mut config = test_run_settings(dir.path(), "lifecycle-test");
        let outcome = engine
            .run_with_lifecycle(&g, &mut config, test_lifecycle(Vec::new()), None)
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);

        let collected = events.lock().unwrap();
        let sandbox_init_count = collected
            .iter()
            .filter(|e| matches!(e, WorkflowRunEvent::SandboxInitialized { .. }))
            .count();
        assert_eq!(
            sandbox_init_count, 1,
            "expected exactly one SandboxInitialized event"
        );
    }

    #[tokio::test]
    async fn run_with_lifecycle_runs_setup_commands() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();

        let events = Arc::new(std::sync::Mutex::new(Vec::<WorkflowRunEvent>::new()));
        let events_clone = events.clone();
        let emitter = EventEmitter::new();
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });

        let engine = WorkflowRunEngine::new(make_registry(), Arc::new(emitter), local_env());
        let mut config = test_run_settings(dir.path(), "setup-test");
        let outcome = engine
            .run_with_lifecycle(
                &g,
                &mut config,
                test_lifecycle(vec!["echo hello".to_string()]),
                None,
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);

        let collected = events.lock().unwrap();
        let setup_started = collected
            .iter()
            .any(|e| matches!(e, WorkflowRunEvent::SetupStarted { .. }));
        let setup_completed = collected
            .iter()
            .any(|e| matches!(e, WorkflowRunEvent::SetupCompleted { .. }));
        assert!(setup_started, "expected SetupStarted event");
        assert!(setup_completed, "expected SetupCompleted event");
    }

    #[tokio::test]
    async fn run_with_lifecycle_setup_failure_aborts_run() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();

        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        let mut config = test_run_settings(dir.path(), "setup-fail-test");
        let result = engine
            .run_with_lifecycle(
                &g,
                &mut config,
                test_lifecycle(vec!["exit 1".to_string()]),
                None,
            )
            .await;
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("Setup command failed"),
            "expected setup failure error, got: {err}"
        );
    }

    #[tokio::test]
    async fn cleanup_sandbox_fires_hook() {
        let engine =
            WorkflowRunEngine::new(make_registry(), Arc::new(EventEmitter::new()), local_env());
        // With preserve=true, cleanup should succeed without error
        let result = engine.cleanup_sandbox("test-run", "test-wf", true).await;
        assert!(result.is_ok());
    }

    /// Handler that returns a retryable error on the first call and succeeds on subsequent calls.
    struct FailOnceThenSucceedHandler {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl HandlerTrait for FailOnceThenSucceedHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &crate::handler::EngineServices,
        ) -> std::result::Result<Outcome, FabroError> {
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n == 0 {
                Err(FabroError::handler("transient failure"))
            } else {
                Ok(Outcome::success())
            }
        }
    }

    #[tokio::test]
    async fn retry_emits_stage_started_per_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("retry_events");
        g.attrs
            .insert("goal".to_string(), AttrValue::String("test".to_string()));

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert(
            "type".to_string(),
            AttrValue::String("fail_once".to_string()),
        );
        // Allow 1 retry → 2 attempts total, use aggressive backoff (500ms) for fast tests
        work.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(1));
        work.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String("aggressive".to_string()),
        );
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let events = Arc::new(std::sync::Mutex::new(Vec::<WorkflowRunEvent>::new()));
        let events_clone = events.clone();
        let emitter = EventEmitter::new();
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });

        let mut registry = make_registry();
        registry.register(
            "fail_once",
            Box::new(FailOnceThenSucceedHandler {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }),
        );

        let engine = WorkflowRunEngine::new(registry, Arc::new(emitter), local_env());
        let config = test_run_settings(dir.path(), "retry-events-test");
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);

        let collected = events.lock().unwrap();
        // Collect all StageStarted events for the "work" node
        let work_started: Vec<_> = collected
            .iter()
            .filter_map(|e| match e {
                WorkflowRunEvent::StageStarted {
                    node_id, attempt, ..
                } if node_id == "work" => Some(*attempt),
                _ => None,
            })
            .collect();
        assert_eq!(
            work_started,
            vec![1, 2],
            "expected StageStarted for attempt 1 and attempt 2, got: {work_started:?}"
        );
    }

    #[tokio::test]
    async fn run_with_lifecycle_emits_events_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();

        let event_names = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let names_clone = event_names.clone();
        let emitter = EventEmitter::new();
        emitter.on_event(move |event| {
            let name = match event {
                WorkflowRunEvent::SandboxInitialized { .. } => "SandboxInitialized",
                WorkflowRunEvent::SetupStarted { .. } => "SetupStarted",
                WorkflowRunEvent::SetupCompleted { .. } => "SetupCompleted",
                WorkflowRunEvent::WorkflowRunStarted { .. } => "WorkflowRunStarted",
                WorkflowRunEvent::WorkflowRunCompleted { .. } => "WorkflowRunCompleted",
                _ => return,
            };
            names_clone.lock().unwrap().push(name.to_string());
        });

        let engine = WorkflowRunEngine::new(make_registry(), Arc::new(emitter), local_env());
        let mut config = test_run_settings(dir.path(), "order-test");
        engine
            .run_with_lifecycle(
                &g,
                &mut config,
                test_lifecycle(vec!["echo ok".to_string()]),
                None,
            )
            .await
            .unwrap();

        let names = event_names.lock().unwrap();
        // SandboxInitialized must come before SetupStarted which comes before WorkflowRunStarted
        let sandbox_idx = names
            .iter()
            .position(|n| n == "SandboxInitialized")
            .expect("SandboxInitialized not found");
        let setup_idx = names
            .iter()
            .position(|n| n == "SetupStarted")
            .expect("SetupStarted not found");
        let run_started_idx = names
            .iter()
            .position(|n| n == "WorkflowRunStarted")
            .expect("WorkflowRunStarted not found");
        assert!(
            sandbox_idx < setup_idx,
            "SandboxInitialized ({sandbox_idx}) should come before SetupStarted ({setup_idx})"
        );
        assert!(
            setup_idx < run_started_idx,
            "SetupStarted ({setup_idx}) should come before WorkflowRunStarted ({run_started_idx})"
        );
    }
}
