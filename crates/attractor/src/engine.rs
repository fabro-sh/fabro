use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use futures::FutureExt;
use rand::Rng;

use crate::checkpoint::Checkpoint;
use crate::condition::evaluate_condition;
use crate::context::Context;
use crate::error::{AttractorError, Result};
use crate::event::{EventEmitter, PipelineEvent};
use crate::graph::{Edge, Graph, Node};
use crate::handler::parallel::ParallelHandler;
use crate::handler::HandlerRegistry;
use crate::interviewer::Interviewer;
use crate::outcome::{Outcome, StageStatus};
use crate::preamble::build_preamble;

/// Convert a Duration's milliseconds to u64, saturating on overflow.
fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

// --- Retry policy types ---

/// Configuration for exponential backoff between retry attempts.
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    pub initial_delay_ms: u64,
    pub backoff_factor: f64,
    pub max_delay_ms: u64,
    pub jitter: bool,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay_ms: 200,
            backoff_factor: 2.0,
            max_delay_ms: 60_000,
            jitter: true,
        }
    }
}

impl BackoffConfig {
    /// Calculate delay for a given attempt (1-indexed).
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let exponent = attempt.saturating_sub(1);
        let initial = f64::from(u32::try_from(self.initial_delay_ms).unwrap_or(u32::MAX));
        let max = f64::from(u32::try_from(self.max_delay_ms).unwrap_or(u32::MAX));
        let exp_i32 = i32::try_from(exponent).unwrap_or(i32::MAX);
        let delay_f64 = initial * self.backoff_factor.powi(exp_i32);
        let capped = delay_f64.min(max);
        let final_ms = if self.jitter {
            let mut rng = rand::thread_rng();
            let jitter_factor: f64 = rng.gen_range(0.5..1.5);
            capped * jitter_factor
        } else {
            capped
        };
        // f64 -> u64: clamp to non-negative, truncate via string-free path
        let ms = if final_ms <= 0.0 {
            0u64
        } else if final_ms >= f64::from(u32::MAX) {
            u64::from(u32::MAX)
        } else {
            // Safe: final_ms is in [0, u32::MAX] so the truncated integer fits in u64
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            { final_ms as u64 }
        };
        std::time::Duration::from_millis(ms)
    }
}

/// Retry policy for node execution.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffConfig,
}

impl RetryPolicy {
    /// No retries -- fail immediately.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            backoff: BackoffConfig::default(),
        }
    }

    /// Standard retry policy: 5 attempts, 200ms initial, 2x factor.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            max_attempts: 5,
            backoff: BackoffConfig {
                initial_delay_ms: 200,
                backoff_factor: 2.0,
                max_delay_ms: 60_000,
                jitter: true,
            },
        }
    }

    /// Aggressive retry: 5 attempts, 500ms initial, 2x factor.
    #[must_use]
    pub fn aggressive() -> Self {
        Self {
            max_attempts: 5,
            backoff: BackoffConfig {
                initial_delay_ms: 500,
                backoff_factor: 2.0,
                max_delay_ms: 60_000,
                jitter: true,
            },
        }
    }

    /// Linear retry: 3 attempts, 500ms fixed delay.
    #[must_use]
    pub fn linear() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffConfig {
                initial_delay_ms: 500,
                backoff_factor: 1.0,
                max_delay_ms: 60_000,
                jitter: true,
            },
        }
    }

    /// Patient retry: 3 attempts, 2000ms initial, 3x factor.
    #[must_use]
    pub fn patient() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffConfig {
                initial_delay_ms: 2000,
                backoff_factor: 3.0,
                max_delay_ms: 60_000,
                jitter: true,
            },
        }
    }
}

/// Build a retry policy from node and graph attributes.
/// If the node has a `retry_policy` attribute naming a preset, use that.
/// Otherwise, fall back to `max_retries` / graph default.
fn build_retry_policy(node: &Node, graph: &Graph) -> RetryPolicy {
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
        .unwrap_or_else(|| graph.default_max_retry());
    // max_retries=0 means 1 attempt (no retries)
    let max_attempts = u32::try_from(max_retries + 1).unwrap_or(1).max(1);
    RetryPolicy {
        max_attempts,
        backoff: BackoffConfig::default(),
    }
}

// --- Fidelity resolution (spec 5.4) ---

/// Resolve the context fidelity for a node, following the precedence:
/// 1. Incoming edge `fidelity` attribute
/// 2. Target node `fidelity` attribute
/// 3. Graph `default_fidelity` attribute
/// 4. Default: "compact"
#[must_use]
pub fn resolve_fidelity(incoming_edge: Option<&Edge>, node: &Node, graph: &Graph) -> String {
    if let Some(edge) = incoming_edge {
        if let Some(f) = edge.fidelity() {
            return f.to_string();
        }
    }
    if let Some(f) = node.fidelity() {
        return f.to_string();
    }
    if let Some(f) = graph.default_fidelity() {
        return f.to_string();
    }
    "compact".to_string()
}

// --- Thread ID resolution (spec 5.4) ---

/// Resolve the thread ID for a node, following the precedence (spec lines 1196-1204):
/// 1. Target node `thread_id` attribute
/// 2. Incoming edge `thread_id` attribute
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
    // Step 1: Node thread_id
    if let Some(tid) = node.thread_id() {
        return Some(tid.to_string());
    }
    // Step 2: Edge thread_id
    if let Some(edge) = incoming_edge {
        if let Some(tid) = edge.thread_id() {
            return Some(tid.to_string());
        }
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

/// Write manifest.json at the start of a pipeline run.
fn write_manifest(logs_root: &Path, graph: &Graph) {
    let pipeline_name = if graph.name.is_empty() {
        "unnamed"
    } else {
        &graph.name
    };
    let manifest = serde_json::json!({
        "pipeline_name": pipeline_name,
        "goal": graph.goal(),
        "start_time": Utc::now().to_rfc3339(),
        "node_count": graph.nodes.len(),
        "edge_count": graph.edges.len(),
    });
    if let Ok(json) = serde_json::to_string_pretty(&manifest) {
        let _ = std::fs::create_dir_all(logs_root);
        let _ = std::fs::write(logs_root.join("manifest.json"), json);
    }
}

/// Write status.json for a completed node into {logs_root}/{node_id}/status.json.
fn write_node_status(logs_root: &Path, node_id: &str, outcome: &Outcome) {
    let node_dir = logs_root.join(node_id);
    let _ = std::fs::create_dir_all(&node_dir);
    let status = serde_json::json!({
        "status": outcome.status.to_string(),
        "notes": outcome.notes,
        "failure_reason": outcome.failure_reason,
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
        if let Some(rest) = s.strip_prefix('[').and_then(|s| {
            s.find(']')
                .map(|i| s[i + 1..].trim_start().to_string())
        }) {
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
        if edge.weight() > best.weight()
            || (edge.weight() == best.weight() && edge.to < best.to)
        {
            best = edge;
        }
    }
    Some(best)
}

/// Select the next edge from a node's outgoing edges (spec Section 3.3).
#[must_use]
pub fn select_edge<'a>(
    node_id: &str,
    outcome: &Outcome,
    context: &Context,
    graph: &'a Graph,
) -> Option<&'a Edge> {
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
        return best_by_weight_then_lexical(&condition_matched);
    }

    // Step 2: Preferred label match
    if let Some(pref) = &outcome.preferred_label {
        let normalized_pref = normalize_label(pref);
        for edge in &edges {
            if let Some(label) = edge.label() {
                if normalize_label(label) == normalized_pref {
                    return Some(edge);
                }
            }
        }
    }

    // Step 3: Suggested next IDs
    for suggested_id in &outcome.suggested_next_ids {
        for edge in &edges {
            if edge.to == *suggested_id {
                return Some(edge);
            }
        }
    }

    // Step 4 & 5: Weight with lexical tiebreak (unconditional edges only)
    let unconditional: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.condition().is_none_or(str::is_empty))
        .copied()
        .collect();
    if !unconditional.is_empty() {
        return best_by_weight_then_lexical(&unconditional);
    }

    // Fallback: any edge
    best_by_weight_then_lexical(&edges)
}

// --- Goal gate enforcement ---

/// Check if all goal gates have been satisfied.
/// Returns Ok(()) if all gates passed, or Err with the failed node ID.
fn check_goal_gates(
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
fn get_retry_target(failed_node_id: &str, graph: &Graph) -> Option<String> {
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
fn is_terminal(node: &Node) -> bool {
    node.shape() == "Msquare"
        || node.handler_type() == Some("exit")
}

// --- Pipeline engine ---

/// Configuration for a pipeline run.
pub struct RunConfig {
    pub logs_root: PathBuf,
    pub cancel_token: Option<Arc<AtomicBool>>,
}

/// The pipeline execution engine.
pub struct PipelineEngine {
    registry: Arc<HandlerRegistry>,
    emitter: Arc<EventEmitter>,
    parallel_handler: ParallelHandler,
    pub interviewer: Option<Arc<dyn Interviewer>>,
}

impl PipelineEngine {
    #[must_use]
    pub fn new(registry: HandlerRegistry, emitter: EventEmitter) -> Self {
        let registry = Arc::new(registry);
        let emitter = Arc::new(emitter);
        let parallel_handler =
            ParallelHandler::new(Arc::clone(&registry), Arc::clone(&emitter));
        Self {
            registry,
            emitter,
            parallel_handler,
            interviewer: None,
        }
    }

    /// Create a new engine with an interviewer for inform() callbacks.
    #[must_use]
    pub fn with_interviewer(
        registry: HandlerRegistry,
        emitter: EventEmitter,
        interviewer: Arc<dyn Interviewer>,
    ) -> Self {
        let registry = Arc::new(registry);
        let emitter = Arc::new(emitter);
        let parallel_handler =
            ParallelHandler::new(Arc::clone(&registry), Arc::clone(&emitter));
        Self {
            registry,
            emitter,
            parallel_handler,
            interviewer: Some(interviewer),
        }
    }

    /// Resolve the handler for a node, returning the parallel handler for
    /// parallel nodes and delegating to the registry for everything else.
    fn resolve_handler(&self, node: &Node) -> &dyn crate::handler::Handler {
        if node.handler_type() == Some("parallel") {
            return &self.parallel_handler;
        }
        self.registry.resolve(node)
    }

    /// Call inform on the interviewer, if one is configured.
    fn inform(&self, message: &str, stage: &str) {
        if let Some(ref interviewer) = self.interviewer {
            // inform is async but we fire-and-forget since it's informational
            let interviewer = Arc::clone(interviewer);
            let message = message.to_string();
            let stage = stage.to_string();
            tokio::spawn(async move {
                interviewer.inform(&message, &stage).await;
            });
        }
    }

    /// Mirror graph-level attributes into the context.
    fn mirror_graph_attributes(graph: &Graph, context: &Context) {
        if !graph.goal().is_empty() {
            context.set("graph.goal", serde_json::json!(graph.goal()));
        }
        for (key, val) in &graph.attrs {
            context.set(
                format!("graph.{key}"),
                serde_json::json!(val.to_string_value()),
            );
        }
    }

    /// Execute a node handler with retry policy.
    /// Returns `(outcome, attempts_used)` where `attempts_used` is the 1-indexed count.
    async fn execute_with_retry(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        logs_root: &Path,
        policy: &RetryPolicy,
        stage_index: usize,
    ) -> Result<(Outcome, u32)> {
        let handler = self.resolve_handler(node);

        let node_timeout = node.timeout();

        for attempt in 1..=policy.max_attempts {
            // Gap #11: Panic safety -- catch panics from handler execution
            let result = {
                let future = handler.execute(node, context, graph, logs_root);
                let panic_safe = AssertUnwindSafe(future).catch_unwind();
                // Gap #2: Timeout enforcement -- wrap with tokio::time::timeout
                let timed_result = if let Some(duration) = node_timeout {
                    match tokio::time::timeout(duration, panic_safe).await {
                        Ok(inner) => inner,
                        Err(_elapsed) => {
                            Ok(Ok(Outcome::fail(format!(
                                "handler timed out after {}ms",
                                duration.as_millis()
                            ))))
                        }
                    }
                } else {
                    panic_safe.await
                };
                match timed_result {
                    Ok(r) => r,
                    Err(panic_payload) => {
                        let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                            format!("handler panicked: {s}")
                        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                            format!("handler panicked: {s}")
                        } else {
                            "handler panicked".to_string()
                        };
                        Err(AttractorError::Handler(msg))
                    }
                }
            };

            let outcome = match result {
                Ok(o) => o,
                Err(e) => {
                    // Gap #7: Check should_retry predicate before retrying
                    if attempt < policy.max_attempts && handler.should_retry(&e) {
                        let delay = policy.backoff.delay_for_attempt(attempt);
                        self.emitter.emit(&PipelineEvent::StageFailed {
                            name: node.label().to_string(),
                            index: stage_index,
                            error: e.to_string(),
                            will_retry: true,
                        });
                        self.emitter.emit(&PipelineEvent::StageRetrying {
                            name: node.label().to_string(),
                            index: stage_index,
                            attempt: usize::try_from(attempt).unwrap_or(usize::MAX),
                            delay_ms: millis_u64(delay),
                        });
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Ok((Outcome::fail(e.to_string()), attempt));
                }
            };

            match outcome.status {
                StageStatus::Success
                | StageStatus::PartialSuccess
                | StageStatus::Fail
                | StageStatus::Skipped => {
                    return Ok((outcome, attempt));
                }
                StageStatus::Retry => {
                    if attempt < policy.max_attempts {
                        let delay = policy.backoff.delay_for_attempt(attempt);
                        self.emitter.emit(&PipelineEvent::StageRetrying {
                            name: node.label().to_string(),
                            index: stage_index,
                            attempt: usize::try_from(attempt).unwrap_or(usize::MAX),
                            delay_ms: millis_u64(delay),
                        });
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    if node.allow_partial() {
                        return Ok((
                            Outcome {
                                status: StageStatus::PartialSuccess,
                                notes: Some(
                                    "retries exhausted, partial accepted".to_string(),
                                ),
                                ..Outcome::success()
                            },
                            attempt,
                        ));
                    }
                    return Ok((Outcome::fail("max retries exceeded"), attempt));
                }
            }
        }

        Ok((Outcome::fail("max retries exceeded"), policy.max_attempts))
    }

    /// Run the pipeline. Returns the final outcome.
    ///
    /// # Errors
    ///
    /// Returns an error if no start node is found, a node is missing, or a goal gate fails
    /// without a retry target.
    pub async fn run(&self, graph: &Graph, config: &RunConfig) -> Result<Outcome> {
        self.run_internal(graph, config, None, None).await
    }

    /// Resume from a checkpoint. Restores context, completed nodes, and continues
    /// execution from the node after the checkpoint's current_node.
    ///
    /// # Errors
    ///
    /// Returns an error if the checkpoint's current node is not found or execution fails.
    pub async fn run_from_checkpoint(
        &self,
        graph: &Graph,
        config: &RunConfig,
        checkpoint: &Checkpoint,
    ) -> Result<Outcome> {
        self.run_internal(graph, config, Some(checkpoint), None).await
    }

    /// Internal run implementation supporting optional checkpoint resume and start_at override.
    #[allow(clippy::too_many_lines)]
    async fn run_internal(
        &self,
        graph: &Graph,
        config: &RunConfig,
        resume_checkpoint: Option<&Checkpoint>,
        start_at: Option<&str>,
    ) -> Result<Outcome> {
        let run_start = Instant::now();
        let run_id = uuid::Uuid::new_v4().to_string();

        self.emitter.emit(&PipelineEvent::PipelineStarted {
            name: graph.name.clone(),
            id: run_id,
        });
        self.inform(&format!("Pipeline started: {}", graph.name), "pipeline");

        // Write manifest.json (spec 5.6)
        write_manifest(&config.logs_root, graph);

        // Gap #4: Initialize from checkpoint, start_at, or fresh
        let context;
        let mut completed_nodes: Vec<String>;
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut node_retries: HashMap<String, u32> = HashMap::new();
        let mut stage_index: usize;
        let mut current_node_id: String;
        let mut incoming_edge: Option<&Edge> = None;
        let mut previous_node_id: Option<String> = None;
        // Gap #6: Track whether fidelity should be degraded on the first resumed node
        let mut degrade_fidelity_on_resume = false;

        if let Some(cp) = resume_checkpoint {
            // Restore context from checkpoint
            context = Context::new();
            for (key, value) in &cp.context_values {
                context.set(key.clone(), value.clone());
            }
            for log_entry in &cp.logs {
                context.append_log(log_entry.clone());
            }
            completed_nodes = cp.completed_nodes.clone();
            // Gap #5: Restore retry counters from checkpoint
            node_retries = cp.node_retries.clone();
            // P1: Restore node outcomes for goal gate checks
            node_outcomes = cp.node_outcomes.clone();
            stage_index = completed_nodes.len();
            // P1: Use stored next_node_id if available, otherwise fall back
            if let Some(ref next_id) = cp.next_node_id {
                current_node_id = next_id.clone();
            } else {
                let edges = graph.outgoing_edges(&cp.current_node);
                if let Some(edge) = edges.first() {
                    current_node_id = edge.to.clone();
                } else {
                    current_node_id = cp.current_node.clone();
                }
            }
            // Gap #6: Check if the checkpointed node used full fidelity
            if cp.context_values.get("internal.fidelity")
                == Some(&serde_json::json!("full"))
            {
                degrade_fidelity_on_resume = true;
            }
        } else if let Some(start) = start_at {
            context = Context::new();
            Self::mirror_graph_attributes(graph, &context);
            completed_nodes = Vec::new();
            stage_index = 0;
            current_node_id = start.to_string();
        } else {
            context = Context::new();
            Self::mirror_graph_attributes(graph, &context);
            completed_nodes = Vec::new();
            stage_index = 0;

            let start_node = graph
                .find_start_node()
                .ok_or_else(|| AttractorError::Engine("no start node found".to_string()))?;
            current_node_id = start_node.id.clone();
        }

        loop {
            // Check for cancellation before processing each node
            if let Some(ref token) = config.cancel_token {
                if token.load(Ordering::Relaxed) {
                    return Err(AttractorError::Cancelled);
                }
            }

            let node = graph.nodes.get(&current_node_id).ok_or_else(|| {
                AttractorError::Engine(format!("node not found: {current_node_id}"))
            })?;

            // Step 1: Check for terminal node
            if is_terminal(node) {
                match check_goal_gates(graph, &node_outcomes) {
                    Ok(()) => break,
                    Err(failed_node_id) => {
                        if let Some(retry_target) =
                            get_retry_target(&failed_node_id, graph)
                        {
                            current_node_id = retry_target;
                            continue;
                        }
                        let duration_ms = millis_u64(run_start.elapsed());
                        let error_msg =
                            format!("goal gate unsatisfied for node {failed_node_id} and no retry target");
                        self.emitter.emit(&PipelineEvent::PipelineFailed {
                            error: error_msg.clone(),
                            duration_ms,
                        });
                        return Err(AttractorError::Engine(error_msg));
                    }
                }
            }

            // Resolve fidelity (spec 5.4) and store in context
            let mut fidelity = resolve_fidelity(incoming_edge, node, graph);
            // Gap #6: On the first node after resume, degrade full -> summary:high
            if degrade_fidelity_on_resume && fidelity == "full" {
                fidelity = "summary:high".to_string();
            }
            degrade_fidelity_on_resume = false;
            context.set("internal.fidelity", serde_json::json!(&fidelity));

            // Preamble injection at execution time (spec 5.4 / 8.3): synthesize a
            // fidelity-appropriate preamble from runtime data for handlers to read
            if fidelity != "full" {
                let preamble = build_preamble(
                    &fidelity,
                    &context,
                    graph,
                    &completed_nodes,
                    &node_outcomes,
                );
                context.set("current.preamble", serde_json::json!(preamble));
            } else {
                context.set("current.preamble", serde_json::json!(""));
            }

            // Thread context sharing: resolve thread ID and store in context for handlers
            let resolved_thread_id = resolve_thread_id(
                incoming_edge,
                node,
                graph,
                previous_node_id.as_deref(),
            );
            if let Some(ref tid) = resolved_thread_id {
                context.set(
                    format!("thread.{tid}.current_node"),
                    serde_json::json!(&node.id),
                );
                context.set("internal.thread_id", serde_json::json!(tid));
            } else {
                context.set("internal.thread_id", serde_json::Value::Null);
            }

            // Step 2: Execute node handler with retry policy
            context.set("current_node", serde_json::json!(&node.id));
            let retry_policy = build_retry_policy(node, graph);

            self.emitter.emit(&PipelineEvent::StageStarted {
                name: node.label().to_string(),
                index: stage_index,
            });
            self.inform(
                &format!("Stage started: {}", node.label()),
                &node.id,
            );
            let stage_start = Instant::now();

            let (mut outcome, attempts_used) = self
                .execute_with_retry(node, &context, graph, &config.logs_root, &retry_policy, stage_index)
                .await?;
            // Gap #5: Track retry count per node
            node_retries.insert(node.id.clone(), attempts_used);
            context.set(
                format!("internal.retry_count.{}", node.id),
                serde_json::json!(attempts_used),
            );

            // Gap #1: Auto status -- when auto_status=true and outcome is non-success,
            // override to success with auto-status note
            if node.auto_status() && outcome.status != StageStatus::Success {
                outcome = Outcome {
                    status: StageStatus::Success,
                    notes: Some("auto-status: handler completed without writing status".to_string()),
                    ..outcome
                };
            }

            let stage_duration_ms = millis_u64(stage_start.elapsed());

            if outcome.status == StageStatus::Fail {
                self.emitter.emit(&PipelineEvent::StageFailed {
                    name: node.label().to_string(),
                    index: stage_index,
                    error: outcome
                        .failure_reason
                        .as_deref()
                        .unwrap_or("unknown")
                        .to_string(),
                    will_retry: false,
                });
            } else {
                self.emitter.emit(&PipelineEvent::StageCompleted {
                    name: node.label().to_string(),
                    index: stage_index,
                    duration_ms: stage_duration_ms,
                });
                self.inform(
                    &format!("Stage completed: {}", node.label()),
                    &node.id,
                );
            }

            // Write per-node status.json (spec 5.6)
            write_node_status(&config.logs_root, &node.id, &outcome);

            // Step 3: Record completion
            completed_nodes.push(node.id.clone());
            node_outcomes.insert(node.id.clone(), outcome.clone());
            previous_node_id = Some(node.id.clone());
            stage_index += 1;

            // Step 4: Apply context updates from outcome
            context.apply_updates(&outcome.context_updates);
            context.set("outcome", serde_json::json!(outcome.status.to_string()));
            if let Some(ref pref) = outcome.preferred_label {
                context.set("preferred_label", serde_json::json!(pref));
            }

            // Step 5: Select next edge (done before checkpoint so we can store next_node_id)
            let next_edge = select_edge(&node.id, &outcome, &context, graph);
            let next_node_id_for_checkpoint = next_edge.map(|e| e.to.clone());

            // Step 6: Save checkpoint with all state
            let checkpoint = Checkpoint::from_context(
                &context,
                &node.id,
                completed_nodes.clone(),
                node_retries.clone(),
                node_outcomes.clone(),
                next_node_id_for_checkpoint,
            );
            let checkpoint_path = config.logs_root.join("checkpoint.json");
            if let Err(e) = checkpoint.save(&checkpoint_path) {
                context.append_log(format!("checkpoint save failed: {e}"));
            } else {
                self.emitter.emit(&PipelineEvent::CheckpointSaved {
                    node_id: node.id.clone(),
                });
            }

            // Step 7: Follow selected edge
            match next_edge {
                None => {
                    // Gap #1: Failure routing -- when FAIL and no matching edge,
                    // check retry_target / fallback_retry_target before terminating
                    if outcome.status == StageStatus::Fail {
                        if let Some(retry_target) = get_retry_target(&node.id, graph) {
                            current_node_id = retry_target;
                            continue;
                        }
                        let duration_ms = millis_u64(run_start.elapsed());
                        let error_msg = format!(
                            "stage {} failed with no outgoing fail edge",
                            node.id
                        );
                        self.emitter.emit(&PipelineEvent::PipelineFailed {
                            error: error_msg.clone(),
                            duration_ms,
                        });
                        return Err(AttractorError::Engine(error_msg));
                    }
                    break;
                }
                Some(edge) => {
                    // Track incoming edge for fidelity resolution on the next node
                    incoming_edge = Some(edge);
                    // Gap #6: Handle loop_restart by recursively running from the target
                    if edge.loop_restart() {
                        return Box::pin(self.run_internal(
                            graph,
                            config,
                            None,
                            Some(&edge.to),
                        )).await;
                    }
                    current_node_id.clone_from(&edge.to);
                }
            }
        }

        let duration_ms = millis_u64(run_start.elapsed());
        self.emitter.emit(&PipelineEvent::PipelineCompleted {
            duration_ms,
            artifact_count: 0,
        });

        // Return last outcome, or success if no outcomes recorded
        let last_outcome = node_outcomes
            .get(completed_nodes.last().unwrap_or(&String::new()))
            .cloned()
            .unwrap_or_else(Outcome::success);
        Ok(last_outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::AttrValue;
    use crate::handler::start::StartHandler;
    use crate::handler::Handler as HandlerTrait;
    use async_trait::async_trait;
    use std::time::Duration;

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
            _logs_root: &Path,
        ) -> std::result::Result<Outcome, AttractorError> {
            Ok(Outcome::fail("always fails"))
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
            _logs_root: &Path,
        ) -> std::result::Result<Outcome, AttractorError> {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            Ok(Outcome::success())
        }
    }

    // --- BackoffConfig tests ---

    #[test]
    fn backoff_no_jitter_first_attempt() {
        let config = BackoffConfig {
            initial_delay_ms: 200,
            backoff_factor: 2.0,
            max_delay_ms: 60_000,
            jitter: false,
        };
        let delay = config.delay_for_attempt(1);
        assert_eq!(delay.as_millis(), 200);
    }

    #[test]
    fn backoff_no_jitter_second_attempt() {
        let config = BackoffConfig {
            initial_delay_ms: 200,
            backoff_factor: 2.0,
            max_delay_ms: 60_000,
            jitter: false,
        };
        let delay = config.delay_for_attempt(2);
        assert_eq!(delay.as_millis(), 400);
    }

    #[test]
    fn backoff_no_jitter_third_attempt() {
        let config = BackoffConfig {
            initial_delay_ms: 200,
            backoff_factor: 2.0,
            max_delay_ms: 60_000,
            jitter: false,
        };
        let delay = config.delay_for_attempt(3);
        assert_eq!(delay.as_millis(), 800);
    }

    #[test]
    fn backoff_respects_max_delay() {
        let config = BackoffConfig {
            initial_delay_ms: 10_000,
            backoff_factor: 10.0,
            max_delay_ms: 30_000,
            jitter: false,
        };
        let delay = config.delay_for_attempt(5);
        assert_eq!(delay.as_millis(), 30_000);
    }

    #[test]
    fn backoff_with_jitter_is_in_range() {
        let config = BackoffConfig {
            initial_delay_ms: 1000,
            backoff_factor: 1.0,
            max_delay_ms: 60_000,
            jitter: true,
        };
        let delay = config.delay_for_attempt(1);
        // With jitter factor 0.5..1.5, delay should be 500..1500
        assert!(delay.as_millis() >= 500);
        assert!(delay.as_millis() <= 1500);
    }

    #[test]
    fn backoff_linear_factor() {
        let config = BackoffConfig {
            initial_delay_ms: 500,
            backoff_factor: 1.0,
            max_delay_ms: 60_000,
            jitter: false,
        };
        assert_eq!(config.delay_for_attempt(1).as_millis(), 500);
        assert_eq!(config.delay_for_attempt(2).as_millis(), 500);
        assert_eq!(config.delay_for_attempt(3).as_millis(), 500);
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
        assert_eq!(policy.backoff.initial_delay_ms, 200);
    }

    #[test]
    fn retry_policy_aggressive() {
        let policy = RetryPolicy::aggressive();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay_ms, 500);
    }

    #[test]
    fn retry_policy_linear() {
        let policy = RetryPolicy::linear();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff.backoff_factor, 1.0);
    }

    #[test]
    fn retry_policy_patient() {
        let policy = RetryPolicy::patient();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff.initial_delay_ms, 2000);
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
            .insert("default_max_retry".to_string(), AttrValue::Integer(2));
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 3); // 2 retries + 1 initial
    }

    #[test]
    fn build_retry_policy_no_attrs_uses_graph_default_50() {
        let node = Node::new("n");
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 51); // default_max_retry=50 + 1
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
        assert_eq!(policy.backoff.initial_delay_ms, 500);
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
        assert_eq!(policy.backoff.initial_delay_ms, 200);
    }

    #[test]
    fn build_retry_policy_all_presets() {
        let presets = vec![
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
        // Unknown preset should fall back to graph default_max_retry=50
        assert_eq!(policy.max_attempts, 51);
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
        e2.attrs
            .insert("weight".to_string(), AttrValue::Integer(5));
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
        let outcome = Outcome::success();
        let context = Context::new();
        assert!(select_edge("a", &outcome, &context, &g).is_none());
    }

    #[test]
    fn select_edge_single_unconditional() {
        let g = make_graph_with_edges(vec![Edge::new("a", "b")]);
        let outcome = Outcome::success();
        let context = Context::new();
        let edge = select_edge("a", &outcome, &context, &g).unwrap();
        assert_eq!(edge.to, "b");
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
        let outcome = Outcome::success();
        let context = Context::new();
        let edge = select_edge("a", &outcome, &context, &g).unwrap();
        assert_eq!(edge.to, "success_path");
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
        let mut outcome = Outcome::success();
        outcome.preferred_label = Some("Fix".to_string());
        let context = Context::new();
        let edge = select_edge("a", &outcome, &context, &g).unwrap();
        assert_eq!(edge.to, "fix");
    }

    #[test]
    fn select_edge_suggested_next_ids() {
        let e1 = Edge::new("a", "path1");
        let e2 = Edge::new("a", "path2");
        let g = make_graph_with_edges(vec![e1, e2]);
        let mut outcome = Outcome::success();
        outcome.suggested_next_ids = vec!["path2".to_string()];
        let context = Context::new();
        let edge = select_edge("a", &outcome, &context, &g).unwrap();
        assert_eq!(edge.to, "path2");
    }

    #[test]
    fn select_edge_weight_tiebreak() {
        let mut e1 = Edge::new("a", "low");
        e1.attrs
            .insert("weight".to_string(), AttrValue::Integer(1));
        let mut e2 = Edge::new("a", "high");
        e2.attrs
            .insert("weight".to_string(), AttrValue::Integer(10));
        let g = make_graph_with_edges(vec![e1, e2]);
        let outcome = Outcome::success();
        let context = Context::new();
        let edge = select_edge("a", &outcome, &context, &g).unwrap();
        assert_eq!(edge.to, "high");
    }

    #[test]
    fn select_edge_lexical_tiebreak() {
        let e1 = Edge::new("a", "charlie");
        let e2 = Edge::new("a", "alpha");
        let g = make_graph_with_edges(vec![e1, e2]);
        let outcome = Outcome::success();
        let context = Context::new();
        let edge = select_edge("a", &outcome, &context, &g).unwrap();
        assert_eq!(edge.to, "alpha");
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
        let outcome = Outcome::success();
        let context = Context::new();
        let edge = select_edge("a", &outcome, &context, &g).unwrap();
        assert_eq!(edge.to, "cond_path");
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
        outcomes.insert("work".to_string(), Outcome::fail("test"));

        assert_eq!(check_goal_gates(&g, &outcomes), Err("work".to_string()));
    }

    #[test]
    fn goal_gates_non_gate_nodes_ignored() {
        let mut g = Graph::new("test");
        g.nodes.insert("work".to_string(), Node::new("work"));

        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), Outcome::fail("test"));

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

        assert_eq!(
            get_retry_target("work", &g),
            Some("plan".to_string())
        );
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

        assert_eq!(
            get_retry_target("work", &g),
            Some("plan".to_string())
        );
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

        assert_eq!(
            get_retry_target("work", &g),
            Some("plan".to_string())
        );
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
        n.attrs.insert(
            "type".to_string(),
            AttrValue::String("exit".to_string()),
        );
        assert!(is_terminal(&n));
    }

    #[test]
    fn non_terminal_node() {
        let n = Node::new("work");
        assert!(!is_terminal(&n));
    }

    // --- PipelineEngine integration tests ---

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
    async fn engine_runs_simple_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn engine_saves_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
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
        let mut emitter = EventEmitter::new();
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(format!("{event:?}"));
        });

        let engine = PipelineEngine::new(make_registry(), emitter);
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        engine.run(&g, &config).await.unwrap();

        let collected = events.lock().unwrap();
        // Should have: PipelineStarted, StageStarted (start), StageCompleted (start),
        // CheckpointSaved, PipelineCompleted
        assert!(collected.len() >= 4);
    }

    #[tokio::test]
    async fn engine_error_when_no_start_node() {
        let dir = tempfile::tempdir().unwrap();
        let g = Graph::new("empty");
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn engine_mirrors_graph_goal_to_context() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        engine.run(&g, &config).await.unwrap();

        // Verify checkpoint has graph.goal mirrored
        let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        assert_eq!(
            cp.context_values.get("graph.goal"),
            Some(&serde_json::json!("Run tests"))
        );
    }

    #[tokio::test]
    async fn engine_multi_node_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = simple_graph();
        // Insert a work node between start and exit
        let work = Node::new("work");
        g.nodes.insert("work".to_string(), work);
        g.edges.clear();
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
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

        g.nodes
            .insert("path_a".to_string(), Node::new("path_a"));
        g.nodes
            .insert("path_b".to_string(), Node::new("path_b"));

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

        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
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
        let node = Node::new("work");
        let graph = Graph::new("test");
        assert_eq!(resolve_fidelity(None, &node, &graph), "compact");
    }

    #[test]
    fn fidelity_from_graph_default() {
        let node = Node::new("work");
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        assert_eq!(resolve_fidelity(None, &node, &graph), "truncate");
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
        assert_eq!(resolve_fidelity(None, &node, &graph), "full");
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
        assert_eq!(resolve_fidelity(Some(&edge), &node, &graph), "summary:high");
    }

    // --- manifest.json and node status tests ---

    #[tokio::test]
    async fn engine_writes_manifest_json() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        engine.run(&g, &config).await.unwrap();

        let manifest_path = dir.path().join("manifest.json");
        assert!(manifest_path.exists());
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["pipeline_name"], "test_pipeline");
        assert_eq!(manifest["goal"], "Run tests");
        assert!(manifest["start_time"].is_string());
        assert!(manifest["node_count"].is_number());
        assert!(manifest["edge_count"].is_number());
    }

    #[tokio::test]
    async fn engine_writes_node_status_json() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        engine.run(&g, &config).await.unwrap();

        // start node should have status.json
        let status_path = dir.path().join("start").join("status.json");
        assert!(status_path.exists());
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(status["status"], "success");
    }

    #[tokio::test]
    async fn engine_stores_fidelity_in_context() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        engine.run(&g, &config).await.unwrap();

        // The checkpoint context should contain internal.fidelity
        let cp = Checkpoint::load(&dir.path().join("checkpoint.json")).unwrap();
        assert_eq!(
            cp.context_values.get("internal.fidelity"),
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
    fn thread_id_node_overrides_edge() {
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
            Some("node-thread".to_string())
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

    // --- Gap #15: Manifest goal field test ---

    #[tokio::test]
    async fn engine_manifest_includes_goal() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig { logs_root: dir.path().to_path_buf(), cancel_token: None };
        engine.run(&g, &config).await.unwrap();

        let manifest_path = dir.path().join("manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["goal"], "Run tests");
    }

    #[tokio::test]
    async fn engine_manifest_goal_empty_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("no_goal");
        let mut start = Node::new("start");
        start.attrs.insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);
        let mut exit = Node::new("exit");
        exit.attrs.insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
        g.nodes.insert("exit".to_string(), exit);
        g.edges.push(Edge::new("start", "exit"));

        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig { logs_root: dir.path().to_path_buf(), cancel_token: None };
        engine.run(&g, &config).await.unwrap();

        let manifest_path = dir.path().join("manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["goal"], "");
    }

    // --- Gap #1: Auto status tests ---

    #[tokio::test]
    async fn engine_auto_status_overrides_fail_to_success() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("auto_status_test");

        let mut start = Node::new("start");
        start.attrs.insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert("auto_status".to_string(), AttrValue::Boolean(true));
        work.attrs.insert("type".to_string(), AttrValue::String("always_fail".to_string()));
        work.attrs.insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(AlwaysFailHandler));
        let engine = PipelineEngine::new(registry, EventEmitter::new());
        let config = RunConfig { logs_root: dir.path().to_path_buf(), cancel_token: None };
        let outcome = engine.run(&g, &config).await.unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
        assert_eq!(
            outcome.notes.as_deref(),
            Some("auto-status: handler completed without writing status")
        );
    }

    #[tokio::test]
    async fn engine_auto_status_false_preserves_fail() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("no_auto_status_test");

        let mut start = Node::new("start");
        start.attrs.insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert("type".to_string(), AttrValue::String("always_fail".to_string()));
        work.attrs.insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        let mut fail_edge = Edge::new("work", "exit");
        fail_edge.attrs.insert("condition".to_string(), AttrValue::String("outcome=fail".to_string()));
        g.edges.push(fail_edge);

        let mut registry = make_registry();
        registry.register("always_fail", Box::new(AlwaysFailHandler));
        let engine = PipelineEngine::new(registry, EventEmitter::new());
        let config = RunConfig { logs_root: dir.path().to_path_buf(), cancel_token: None };
        let result = engine.run(&g, &config).await;

        assert!(result.is_ok());
        let status_path = dir.path().join("work").join("status.json");
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
        start.attrs.insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert("timeout".to_string(), AttrValue::Duration(Duration::from_millis(50)));
        work.attrs.insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs.insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        let mut fail_edge = Edge::new("work", "exit");
        fail_edge.attrs.insert("condition".to_string(), AttrValue::String("outcome=fail".to_string()));
        g.edges.push(fail_edge);

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 500 }));
        let engine = PipelineEngine::new(registry, EventEmitter::new());
        let config = RunConfig { logs_root: dir.path().to_path_buf(), cancel_token: None };
        let result = engine.run(&g, &config).await;
        assert!(result.is_ok());

        let status_path = dir.path().join("work").join("status.json");
        let status: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(status["status"], "fail");
    }

    #[tokio::test]
    async fn engine_no_timeout_completes_normally() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("no_timeout_test");

        let mut start = Node::new("start");
        start.attrs.insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs.insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 10 }));
        let engine = PipelineEngine::new(registry, EventEmitter::new());
        let config = RunConfig { logs_root: dir.path().to_path_buf(), cancel_token: None };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn engine_timeout_with_auto_status_returns_success() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Graph::new("timeout_auto_status_test");

        let mut start = Node::new("start");
        start.attrs.insert("shape".to_string(), AttrValue::String("Mdiamond".to_string()));
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs.insert("timeout".to_string(), AttrValue::Duration(Duration::from_millis(50)));
        work.attrs.insert("auto_status".to_string(), AttrValue::Boolean(true));
        work.attrs.insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs.insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert("shape".to_string(), AttrValue::String("Msquare".to_string()));
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 500 }));
        let engine = PipelineEngine::new(registry, EventEmitter::new());
        let config = RunConfig { logs_root: dir.path().to_path_buf(), cancel_token: None };
        let outcome = engine.run(&g, &config).await.unwrap();

        assert_eq!(outcome.status, StageStatus::Success);
        assert_eq!(
            outcome.notes.as_deref(),
            Some("auto-status: handler completed without writing status")
        );
    }

    // --- Gap #15: Interviewer.inform() tests ---

    /// Mock interviewer that records inform() calls.
    struct RecordingInformer {
        messages: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl RecordingInformer {
        fn new() -> Self {
            Self {
                messages: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl crate::interviewer::Interviewer for RecordingInformer {
        async fn ask(&self, _question: crate::interviewer::Question) -> crate::interviewer::Answer {
            crate::interviewer::Answer::yes()
        }

        async fn inform(&self, message: &str, stage: &str) {
            self.messages
                .lock()
                .unwrap()
                .push((message.to_string(), stage.to_string()));
        }
    }

    #[tokio::test]
    async fn engine_calls_inform_on_pipeline_start() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let informer = Arc::new(RecordingInformer::new());
        let engine = PipelineEngine::with_interviewer(
            make_registry(),
            EventEmitter::new(),
            Arc::clone(&informer) as Arc<dyn crate::interviewer::Interviewer>,
        );
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        engine.run(&g, &config).await.unwrap();
        // Give spawned inform tasks time to complete
        tokio::time::sleep(Duration::from_millis(50)).await;

        let messages = informer.messages.lock().unwrap();
        assert!(
            messages.iter().any(|(msg, stage)| msg.contains("Pipeline started") && stage == "pipeline"),
            "expected 'Pipeline started' inform call, got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn engine_calls_inform_on_stage_start_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let informer = Arc::new(RecordingInformer::new());
        let engine = PipelineEngine::with_interviewer(
            make_registry(),
            EventEmitter::new(),
            Arc::clone(&informer) as Arc<dyn crate::interviewer::Interviewer>,
        );
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        engine.run(&g, &config).await.unwrap();
        // Give spawned inform tasks time to complete
        tokio::time::sleep(Duration::from_millis(50)).await;

        let messages = informer.messages.lock().unwrap();
        assert!(
            messages.iter().any(|(msg, _)| msg.contains("Stage started")),
            "expected 'Stage started' inform call, got: {messages:?}"
        );
        assert!(
            messages.iter().any(|(msg, _)| msg.contains("Stage completed")),
            "expected 'Stage completed' inform call, got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn engine_without_interviewer_runs_normally() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: None,
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    // --- Gap #7: Cancellation token tests ---

    #[tokio::test]
    async fn engine_returns_cancelled_when_token_set_before_run() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let cancel_token = Arc::new(AtomicBool::new(true));
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: Some(cancel_token),
        };
        let result = engine.run(&g, &config).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AttractorError::Cancelled));
    }

    #[tokio::test]
    async fn engine_runs_normally_with_unset_cancel_token() {
        let dir = tempfile::tempdir().unwrap();
        let g = simple_graph();
        let engine = PipelineEngine::new(make_registry(), EventEmitter::new());
        let cancel_token = Arc::new(AtomicBool::new(false));
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: Some(cancel_token),
        };
        let outcome = engine.run(&g, &config).await.unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn engine_cancelled_mid_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = simple_graph();
        // Insert a work node between start and exit
        let mut work = Node::new("work");
        work.attrs.insert("type".to_string(), AttrValue::String("slow".to_string()));
        work.attrs.insert("max_retries".to_string(), AttrValue::Integer(0));
        g.nodes.insert("work".to_string(), work);
        g.edges.clear();
        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));

        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancel_token_clone = Arc::clone(&cancel_token);

        let mut registry = make_registry();
        registry.register("slow", Box::new(SlowHandler { sleep_ms: 200 }));
        let engine = PipelineEngine::new(registry, EventEmitter::new());
        let config = RunConfig {
            logs_root: dir.path().to_path_buf(),
            cancel_token: Some(cancel_token),
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
        assert!(matches!(result.unwrap_err(), AttractorError::Cancelled));
    }
}