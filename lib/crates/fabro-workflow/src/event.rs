use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use fabro_store::{EventPayload, RunStore};
use fabro_types::RunId;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::error::FabroError;
use crate::outcome::{FailureDetail, StageUsage};
use fabro_agent::{AgentEvent, SandboxEvent, WorktreeEvent, WorktreeEventCallback};
use fabro_llm::types::Usage as LlmUsage;
use fabro_util::redact::redact_jsonl_line;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunNoticeLevel {
    Info,
    Warn,
    Error,
}

/// Events emitted during workflow run execution for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowRunEvent {
    WorkflowRunStarted {
        name: String,
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal: Option<String>,
    },
    WorkflowRunCompleted {
        duration_ms: u64,
        artifact_count: usize,
        #[serde(default)]
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_cost: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_git_commit_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<LlmUsage>,
    },
    WorkflowRunFailed {
        error: FabroError,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_commit_sha: Option<String>,
    },
    RunNotice {
        level: RunNoticeLevel,
        code: String,
        message: String,
    },
    StageStarted {
        node_id: String,
        name: String,
        index: usize,
        handler_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script: Option<String>,
        attempt: usize,
        max_attempts: usize,
    },
    StageCompleted {
        node_id: String,
        name: String,
        index: usize,
        duration_ms: u64,
        status: String,
        preferred_label: Option<String>,
        suggested_next_ids: Vec<String>,
        usage: Option<StageUsage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<FailureDetail>,
        notes: Option<String>,
        files_touched: Vec<String>,
        attempt: usize,
        max_attempts: usize,
    },
    StageFailed {
        node_id: String,
        name: String,
        index: usize,
        failure: FailureDetail,
        will_retry: bool,
    },
    StageRetrying {
        node_id: String,
        name: String,
        index: usize,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
    },
    ParallelStarted {
        branch_count: usize,
        join_policy: String,
    },
    ParallelBranchStarted {
        branch: String,
        index: usize,
    },
    ParallelBranchCompleted {
        branch: String,
        index: usize,
        duration_ms: u64,
        status: String,
    },
    ParallelCompleted {
        duration_ms: u64,
        success_count: usize,
        failure_count: usize,
    },
    InterviewStarted {
        question: String,
        stage: String,
        question_type: String,
    },
    InterviewCompleted {
        question: String,
        answer: String,
        duration_ms: u64,
    },
    InterviewTimeout {
        question: String,
        stage: String,
        duration_ms: u64,
    },
    CheckpointCompleted {
        node_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_commit_sha: Option<String>,
    },
    CheckpointFailed {
        node_id: String,
        error: String,
    },
    GitCommit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        sha: String,
    },
    GitPush {
        branch: String,
        success: bool,
    },
    GitBranch {
        branch: String,
        sha: String,
    },
    GitWorktreeAdd {
        path: String,
        branch: String,
    },
    GitWorktreeRemove {
        path: String,
    },
    GitFetch {
        branch: String,
        success: bool,
    },
    GitReset {
        sha: String,
    },
    EdgeSelected {
        from_node: String,
        to_node: String,
        label: Option<String>,
        condition: Option<String>,
        /// Which selection step chose this edge (e.g. "condition", "preferred_label", "jump").
        reason: String,
        /// The stage's preferred label hint, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preferred_label: Option<String>,
        /// The stage's suggested next node IDs, if any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggested_next_ids: Vec<String>,
        /// The stage outcome status that influenced routing.
        stage_status: String,
        /// Whether this was a direct jump (bypassing normal edge selection).
        is_jump: bool,
    },
    LoopRestart {
        from_node: String,
        to_node: String,
    },
    Prompt {
        stage: String,
        text: String,
    },
    /// Forwarded from an agent session, tagged with the workflow stage.
    Agent {
        stage: String,
        event: AgentEvent,
    },
    SubgraphStarted {
        node_id: String,
        start_node: String,
    },
    SubgraphCompleted {
        node_id: String,
        steps_executed: usize,
        status: String,
        duration_ms: u64,
    },
    /// Forwarded from a sandbox lifecycle operation.
    Sandbox {
        event: SandboxEvent,
    },
    /// Emitted after the sandbox has been initialized (by engine lifecycle).
    SandboxInitialized {
        working_directory: String,
    },
    SetupStarted {
        command_count: usize,
    },
    SetupCommandStarted {
        command: String,
        index: usize,
    },
    SetupCommandCompleted {
        command: String,
        index: usize,
        exit_code: i32,
        duration_ms: u64,
    },
    SetupCompleted {
        duration_ms: u64,
    },
    SetupFailed {
        command: String,
        index: usize,
        exit_code: i32,
        stderr: String,
    },
    StallWatchdogTimeout {
        node: String,
        idle_seconds: u64,
    },
    AssetCaptured {
        node_id: String,
        attempt: u32,
        node_slug: String,
        path: String,
        mime: String,
        content_md5: String,
        content_sha256: String,
        bytes: u64,
    },
    SshAccessReady {
        ssh_command: String,
    },
    Failover {
        stage: String,
        from_provider: String,
        from_model: String,
        to_provider: String,
        to_model: String,
        error: String,
    },
    CliEnsureStarted {
        cli_name: String,
        provider: String,
    },
    CliEnsureCompleted {
        cli_name: String,
        provider: String,
        already_installed: bool,
        node_installed: bool,
        duration_ms: u64,
    },
    CliEnsureFailed {
        cli_name: String,
        provider: String,
        error: String,
        duration_ms: u64,
    },
    PullRequestCreated {
        pr_url: String,
        pr_number: u64,
        draft: bool,
    },
    PullRequestFailed {
        error: String,
    },
    DevcontainerResolved {
        dockerfile_lines: usize,
        environment_count: usize,
        lifecycle_command_count: usize,
        workspace_folder: String,
    },
    DevcontainerLifecycleStarted {
        phase: String,
        command_count: usize,
    },
    DevcontainerLifecycleCommandStarted {
        phase: String,
        command: String,
        index: usize,
    },
    DevcontainerLifecycleCommandCompleted {
        phase: String,
        command: String,
        index: usize,
        exit_code: i32,
        duration_ms: u64,
    },
    DevcontainerLifecycleCompleted {
        phase: String,
        duration_ms: u64,
    },
    DevcontainerLifecycleFailed {
        phase: String,
        command: String,
        index: usize,
        exit_code: i32,
        stderr: String,
    },
    RetroStarted,
    RetroCompleted {
        duration_ms: u64,
    },
    RetroFailed {
        error: String,
        duration_ms: u64,
    },
}

impl WorkflowRunEvent {
    pub fn trace(&self) {
        use tracing::{debug, error, info, warn};
        match self {
            Self::WorkflowRunStarted { name, run_id, .. } => {
                info!(workflow = name.as_str(), run_id = %run_id, "Workflow run started");
            }
            Self::WorkflowRunCompleted {
                duration_ms,
                artifact_count,
                status,
                ..
            } => {
                info!(
                    duration_ms,
                    artifact_count, status, "Workflow run completed"
                );
            }
            Self::WorkflowRunFailed {
                error, duration_ms, ..
            } => {
                error!(error = %error, duration_ms, "Workflow run failed");
            }
            Self::RunNotice {
                level,
                code,
                message,
            } => match level {
                RunNoticeLevel::Info => {
                    info!(code, message, "Run notice");
                }
                RunNoticeLevel::Warn => {
                    warn!(code, message, "Run notice");
                }
                RunNoticeLevel::Error => {
                    error!(code, message, "Run notice");
                }
            },
            Self::StageStarted {
                node_id,
                name,
                index,
                handler_type,
                attempt,
                max_attempts,
                ..
            } => {
                debug!(
                    node_id,
                    stage = name.as_str(),
                    index,
                    handler_type = handler_type.as_deref().unwrap_or(""),
                    attempt,
                    max_attempts,
                    "Stage started"
                );
            }
            Self::StageCompleted {
                node_id,
                name,
                index,
                duration_ms,
                status,
                attempt,
                max_attempts,
                ..
            } => {
                debug!(
                    node_id,
                    stage = name.as_str(),
                    index,
                    duration_ms,
                    status,
                    attempt,
                    max_attempts,
                    "Stage completed"
                );
            }
            Self::StageFailed {
                node_id,
                name,
                index,
                failure,
                will_retry,
            } => {
                let error_msg = &failure.message;
                if *will_retry {
                    warn!(
                        node_id,
                        stage = name.as_str(),
                        index,
                        error = error_msg.as_str(),
                        will_retry,
                        "Stage failed"
                    );
                } else {
                    error!(
                        node_id,
                        stage = name.as_str(),
                        index,
                        error = error_msg.as_str(),
                        will_retry,
                        "Stage failed"
                    );
                }
            }
            Self::StageRetrying {
                node_id,
                name,
                index,
                attempt,
                max_attempts,
                delay_ms,
            } => {
                warn!(
                    node_id,
                    stage = name.as_str(),
                    index,
                    attempt,
                    max_attempts,
                    delay_ms,
                    "Stage retrying"
                );
            }
            Self::ParallelStarted {
                branch_count,
                join_policy,
            } => {
                debug!(branch_count, join_policy, "Parallel execution started");
            }
            Self::ParallelBranchStarted { branch, index } => {
                debug!(branch, index, "Parallel branch started");
            }
            Self::ParallelBranchCompleted {
                branch,
                index,
                duration_ms,
                status,
            } => {
                debug!(
                    branch,
                    index, duration_ms, status, "Parallel branch completed"
                );
            }
            Self::ParallelCompleted {
                duration_ms,
                success_count,
                failure_count,
            } => {
                debug!(
                    duration_ms,
                    success_count, failure_count, "Parallel execution completed"
                );
            }
            Self::InterviewStarted {
                stage,
                question_type,
                ..
            } => {
                debug!(stage, question_type, "Interview started");
            }
            Self::InterviewCompleted { duration_ms, .. } => {
                debug!(duration_ms, "Interview completed");
            }
            Self::InterviewTimeout {
                stage, duration_ms, ..
            } => {
                warn!(stage, duration_ms, "Interview timeout");
            }
            Self::CheckpointCompleted {
                node_id, status, ..
            } => {
                debug!(node_id, status, "Checkpoint completed");
            }
            Self::CheckpointFailed { node_id, error } => {
                error!(node_id, error, "Checkpoint failed");
            }
            Self::GitCommit { node_id, sha } => {
                debug!(
                    node_id = node_id.as_deref().unwrap_or(""),
                    sha, "Git commit"
                );
            }
            Self::GitPush { branch, success } => {
                if *success {
                    debug!(branch, "Git push succeeded");
                } else {
                    warn!(branch, "Git push failed");
                }
            }
            Self::GitBranch { branch, sha } => {
                debug!(branch, sha, "Git branch created");
            }
            Self::GitWorktreeAdd { path, branch } => {
                debug!(path, branch, "Git worktree added");
            }
            Self::GitWorktreeRemove { path } => {
                debug!(path, "Git worktree removed");
            }
            Self::GitFetch { branch, success } => {
                if *success {
                    debug!(branch, "Git fetch succeeded");
                } else {
                    warn!(branch, "Git fetch failed");
                }
            }
            Self::GitReset { sha } => {
                debug!(sha, "Git reset");
            }
            Self::EdgeSelected {
                from_node,
                to_node,
                label,
                reason,
                ..
            } => {
                debug!(
                    from_node,
                    to_node,
                    label = label.as_deref().unwrap_or(""),
                    reason,
                    "Edge selected"
                );
            }
            Self::LoopRestart { from_node, to_node } => {
                debug!(from_node, to_node, "Loop restart");
            }
            Self::Prompt { stage, text } => {
                debug!(stage, text_len = text.len(), "Prompt sent");
            }
            Self::Agent { .. } | Self::Sandbox { .. } => {}
            Self::SandboxInitialized {
                working_directory, ..
            } => {
                info!(working_directory, "Sandbox initialized");
            }
            Self::SubgraphStarted {
                node_id,
                start_node,
            } => {
                debug!(node_id, start_node, "Subgraph started");
            }
            Self::SubgraphCompleted {
                node_id,
                steps_executed,
                status,
                duration_ms,
            } => {
                debug!(
                    node_id,
                    steps_executed, status, duration_ms, "Subgraph completed"
                );
            }
            Self::SetupStarted { command_count } => {
                info!(command_count, "Setup started");
            }
            Self::SetupCommandStarted { command, index } => {
                debug!(command, index, "Setup command started");
            }
            Self::SetupCommandCompleted {
                command,
                index,
                exit_code,
                duration_ms,
            } => {
                debug!(
                    command,
                    index, exit_code, duration_ms, "Setup command completed"
                );
            }
            Self::SetupCompleted { duration_ms } => {
                info!(duration_ms, "Setup completed");
            }
            Self::SetupFailed {
                command,
                index,
                exit_code,
                ..
            } => {
                error!(command, index, exit_code, "Setup command failed");
            }
            Self::StallWatchdogTimeout { node, idle_seconds } => {
                warn!(node, idle_seconds, "Stall watchdog timeout");
            }
            Self::AssetCaptured {
                node_id,
                node_slug,
                attempt,
                path,
                bytes,
                ..
            } => {
                debug!(node_id, node_slug, attempt, path, bytes, "Asset captured");
            }
            Self::SshAccessReady { ssh_command } => {
                info!(ssh_command, "SSH access ready");
            }
            Self::Failover {
                stage,
                from_provider,
                from_model,
                to_provider,
                to_model,
                error,
            } => {
                warn!(
                    stage,
                    from_provider,
                    from_model,
                    to_provider,
                    to_model,
                    error,
                    "LLM provider failover"
                );
            }
            Self::CliEnsureStarted {
                cli_name, provider, ..
            } => {
                debug!(cli_name, provider, "CLI ensure started");
            }
            Self::CliEnsureCompleted {
                cli_name,
                provider,
                already_installed,
                node_installed,
                duration_ms,
            } => {
                info!(
                    cli_name,
                    provider,
                    already_installed,
                    node_installed,
                    duration_ms,
                    "CLI ensure completed"
                );
            }
            Self::CliEnsureFailed {
                cli_name,
                provider,
                error,
                duration_ms,
            } => {
                error!(cli_name, provider, error, duration_ms, "CLI ensure failed");
            }
            Self::PullRequestCreated {
                pr_url,
                pr_number,
                draft,
                ..
            } => {
                info!(pr_url = %pr_url, pr_number, draft, "Pull request created");
            }
            Self::PullRequestFailed { error, .. } => {
                error!(error = %error, "Pull request creation failed");
            }
            Self::DevcontainerResolved {
                dockerfile_lines,
                environment_count,
                lifecycle_command_count,
                workspace_folder,
            } => {
                info!(
                    dockerfile_lines,
                    environment_count,
                    lifecycle_command_count,
                    workspace_folder,
                    "Devcontainer resolved"
                );
            }
            Self::DevcontainerLifecycleStarted {
                phase,
                command_count,
            } => {
                info!(phase, command_count, "Devcontainer lifecycle started");
            }
            Self::DevcontainerLifecycleCommandStarted {
                phase,
                command,
                index,
            } => {
                debug!(
                    phase,
                    command, index, "Devcontainer lifecycle command started"
                );
            }
            Self::DevcontainerLifecycleCommandCompleted {
                phase,
                command,
                index,
                exit_code,
                duration_ms,
            } => {
                debug!(
                    phase,
                    command,
                    index,
                    exit_code,
                    duration_ms,
                    "Devcontainer lifecycle command completed"
                );
            }
            Self::DevcontainerLifecycleCompleted { phase, duration_ms } => {
                info!(phase, duration_ms, "Devcontainer lifecycle completed");
            }
            Self::DevcontainerLifecycleFailed {
                phase,
                command,
                index,
                exit_code,
                ..
            } => {
                error!(
                    phase,
                    command, index, exit_code, "Devcontainer lifecycle command failed"
                );
            }
            Self::RetroStarted => {
                info!("Retro started");
            }
            Self::RetroCompleted { duration_ms } => {
                info!(duration_ms, "Retro completed");
            }
            Self::RetroFailed { error, duration_ms } => {
                error!(error = %error, duration_ms, "Retro failed");
            }
        }
    }
}

/// Flatten a `WorkflowRunEvent` into its event name and a map of top-level fields.
///
/// Simple variants like `StageStarted` return `("StageStarted", {fields})`.
/// Wrapper variants use dot notation:
/// - `Agent { stage, event: ToolCallStarted { .. } }` → `"Agent.ToolCallStarted"`
/// - `Sandbox { event: Initializing { .. } }` → `"Sandbox.Initializing"`
/// - `Agent { stage, event: SubAgentEvent { event: inner, .. } }` → `"Agent.SubAgentEvent.{Inner}"`
///   with one level of flattening; deeper nesting stays as JSON.
pub fn flatten_event(
    event: &WorkflowRunEvent,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    let value = serde_json::to_value(event).expect("WorkflowRunEvent must serialize");
    let (event_name, mut fields) = match value {
        serde_json::Value::Object(map) => {
            // Externally-tagged enum: { "VariantName": { fields } }
            let (variant_name, inner) = map.into_iter().next().expect("enum must have one key");
            match variant_name.as_str() {
                "Agent" => flatten_agent(inner),
                "Sandbox" => flatten_sandbox(inner),
                _ => {
                    let fields = match inner {
                        serde_json::Value::Object(m) => m,
                        _ => serde_json::Map::new(),
                    };
                    (variant_name, fields)
                }
            }
        }
        // Unit variants serialize as strings
        serde_json::Value::String(name) => (name, serde_json::Map::new()),
        _ => ("Unknown".to_string(), serde_json::Map::new()),
    };
    rename_fields(&event_name, &mut fields);
    (event_name, fields)
}

pub fn build_event_envelope(event: &WorkflowRunEvent, run_id: &RunId) -> serde_json::Value {
    let (event_name, event_fields) = flatten_event(event);
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "ts".to_string(),
        serde_json::Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
    );
    envelope.insert(
        "run_id".to_string(),
        serde_json::Value::String(run_id.to_string()),
    );
    envelope.insert("event".to_string(), serde_json::Value::String(event_name));
    for (k, v) in event_fields {
        if k != "ts" && k != "run_id" && k != "event" {
            envelope.insert(k, v);
        }
    }
    serde_json::Value::Object(envelope)
}

pub fn build_redacted_event_payload(
    event: &WorkflowRunEvent,
    run_id: &RunId,
) -> Result<EventPayload> {
    let envelope = build_event_envelope(event, run_id);
    let line = serde_json::to_string(&envelope)?;
    let line = redact_jsonl_line(&line);
    let value = serde_json::from_str(&line).context("Failed to parse redacted event payload")?;
    EventPayload::new(value, run_id).map_err(anyhow::Error::from)
}

pub fn append_progress_event(
    run_dir: &Path,
    run_id: &RunId,
    event: &WorkflowRunEvent,
) -> Result<()> {
    let envelope = build_event_envelope(event, run_id);
    let line = serde_json::to_string(&envelope)?;
    let line = redact_jsonl_line(&line);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("progress.jsonl"))
        .with_context(|| {
            format!(
                "Failed to open {}",
                run_dir.join("progress.jsonl").display()
            )
        })?;
    writeln!(file, "{line}")?;

    let pretty = serde_json::to_string_pretty(&envelope)?;
    let pretty = redact_jsonl_line(&pretty);
    std::fs::write(run_dir.join("live.json"), pretty)
        .with_context(|| format!("Failed to write {}", run_dir.join("live.json").display()))?;

    Ok(())
}

pub struct ProgressLogger {
    run_dir: PathBuf,
    run_id: RunId,
}

impl ProgressLogger {
    #[must_use]
    pub fn new(run_dir: impl Into<PathBuf>, run_id: RunId) -> Self {
        Self {
            run_dir: run_dir.into(),
            run_id,
        }
    }

    pub fn register(self, emitter: &EventEmitter) {
        let run_dir = self.run_dir;
        let run_id = Arc::new(std::sync::Mutex::new(self.run_id));
        emitter.on_event(move |event| {
            if let WorkflowRunEvent::WorkflowRunStarted {
                run_id: started_run_id,
                ..
            } = event
            {
                (*run_id.lock().unwrap()).clone_from(started_run_id);
            }
            let _ = append_progress_event(&run_dir, &run_id.lock().unwrap(), event);
        });
    }
}

enum StoreProgressCommand {
    Event(EventPayload),
    Flush(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct StoreProgressLogger {
    tx: mpsc::UnboundedSender<StoreProgressCommand>,
    run_id: Arc<std::sync::Mutex<RunId>>,
}

impl StoreProgressLogger {
    #[must_use]
    pub fn new(run_store: Arc<dyn RunStore>, run_id: RunId) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    StoreProgressCommand::Event(payload) => {
                        if let Err(err) = run_store.append_event(&payload).await {
                            tracing::warn!(error = %err, "Failed to append event to run store");
                        }
                    }
                    StoreProgressCommand::Flush(tx) => {
                        let _ = tx.send(());
                    }
                }
            }
        });

        Self {
            tx,
            run_id: Arc::new(std::sync::Mutex::new(run_id)),
        }
    }

    pub fn register(&self, emitter: &EventEmitter) {
        let tx = self.tx.clone();
        let run_id = Arc::clone(&self.run_id);
        emitter.on_event(move |event| {
            if let WorkflowRunEvent::WorkflowRunStarted {
                run_id: started_run_id,
                ..
            } = event
            {
                (*run_id.lock().unwrap()).clone_from(started_run_id);
            }

            let run_id = *run_id.lock().unwrap();
            match build_redacted_event_payload(event, &run_id) {
                Ok(payload) => {
                    if tx.send(StoreProgressCommand::Event(payload)).is_err() {
                        tracing::warn!(
                            "Store progress logger channel closed while appending event"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "Failed to build store event payload");
                }
            }
        });
    }

    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(StoreProgressCommand::Flush(tx)).is_err() {
            tracing::warn!("Store progress logger channel closed before flush");
            return;
        }
        if rx.await.is_err() {
            tracing::warn!("Store progress logger flush dropped before completion");
        }
    }
}

fn flatten_agent(inner: serde_json::Value) -> (String, serde_json::Map<String, serde_json::Value>) {
    let serde_json::Value::Object(mut agent_fields) = inner else {
        return ("Agent".to_string(), serde_json::Map::new());
    };
    let stage = agent_fields.remove("stage");
    let agent_event = agent_fields
        .remove("event")
        .unwrap_or(serde_json::Value::Null);

    match agent_event {
        serde_json::Value::Object(event_map) => {
            let (inner_name, inner_value) = event_map
                .into_iter()
                .next()
                .expect("agent event must have one key");
            if inner_name == "SubAgentEvent" {
                flatten_sub_agent_event(stage, inner_value)
            } else {
                let mut fields = match inner_value {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                if let Some(s) = stage {
                    fields.insert("stage".to_string(), s);
                }
                (format!("Agent.{inner_name}"), fields)
            }
        }
        // Unit variant inside Agent (e.g. SessionStarted)
        serde_json::Value::String(name) => {
            let mut fields = serde_json::Map::new();
            if let Some(s) = stage {
                fields.insert("stage".to_string(), s);
            }
            (format!("Agent.{name}"), fields)
        }
        _ => {
            let mut fields = serde_json::Map::new();
            if let Some(s) = stage {
                fields.insert("stage".to_string(), s);
            }
            ("Agent".to_string(), fields)
        }
    }
}

fn flatten_sandbox(
    inner: serde_json::Value,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    let serde_json::Value::Object(mut sandbox_fields) = inner else {
        return ("Sandbox".to_string(), serde_json::Map::new());
    };
    let sandbox_event = sandbox_fields
        .remove("event")
        .unwrap_or(serde_json::Value::Null);

    match sandbox_event {
        serde_json::Value::Object(event_map) => {
            let (inner_name, inner_value) = event_map
                .into_iter()
                .next()
                .expect("sandbox event must have one key");
            let fields = match inner_value {
                serde_json::Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            (format!("Sandbox.{inner_name}"), fields)
        }
        serde_json::Value::String(name) => (format!("Sandbox.{name}"), serde_json::Map::new()),
        _ => ("Sandbox".to_string(), serde_json::Map::new()),
    }
}

fn flatten_sub_agent_event(
    stage: Option<serde_json::Value>,
    inner_value: serde_json::Value,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    let serde_json::Value::Object(mut sub_fields) = inner_value else {
        let mut fields = serde_json::Map::new();
        if let Some(s) = stage {
            fields.insert("stage".to_string(), s);
        }
        return ("Agent.SubAgentEvent".to_string(), fields);
    };

    // Extract the inner event name for dot notation, but keep full inner
    // event as `nested_event` JSON to avoid field collisions when sub-agents
    // are themselves nested (SubAgentEvent wrapping SubAgentEvent).
    let nested_event = sub_fields
        .remove("event")
        .unwrap_or(serde_json::Value::Null);
    let inner_name = match &nested_event {
        serde_json::Value::Object(map) => map.keys().next().cloned(),
        serde_json::Value::String(name) => Some(name.clone()),
        _ => None,
    };

    let event_name = match &inner_name {
        Some(name) => format!("Agent.SubAgentEvent.{name}"),
        None => "Agent.SubAgentEvent".to_string(),
    };

    // Start with the SubAgentEvent's own fields (agent_id, depth)
    let mut fields = sub_fields;
    if let Some(s) = stage {
        fields.insert("stage".to_string(), s);
    }
    fields.insert("nested_event".to_string(), nested_event);

    (event_name, fields)
}

/// Rename flattened event fields for clarity in progress.jsonl output.
///
/// Applied as a post-processing step after `flatten_event` serialization to
/// give fields self-describing names without changing the Rust enum.
fn rename_fields(event_name: &str, fields: &mut serde_json::Map<String, serde_json::Value>) {
    /// Move a key from `old` to `new` if present.
    fn rename(fields: &mut serde_json::Map<String, serde_json::Value>, old: &str, new: &str) {
        if let Some(v) = fields.remove(old) {
            fields.insert(new.to_string(), v);
        }
    }

    /// Insert `node_label` defaulting to the value of `node_id`, if not already present.
    fn default_node_label(fields: &mut serde_json::Map<String, serde_json::Value>) {
        if !fields.contains_key("node_label") {
            if let Some(id) = fields.get("node_id").cloned() {
                fields.insert("node_label".to_string(), id);
            }
        }
    }

    if event_name.starts_with("Stage") {
        // name → node_label, index → stage_index, node_id stays
        rename(fields, "name", "node_label");
        rename(fields, "index", "stage_index");
        // Flatten FailureDetail into top-level fields for backward compat
        if let Some(serde_json::Value::Object(failure)) = fields.remove("failure") {
            if let Some(msg) = failure.get("message") {
                fields.insert("error".to_string(), msg.clone());
                fields.insert("failure_reason".to_string(), msg.clone());
            }
            if let Some(fc) = failure.get("failure_class") {
                fields.insert("failure_class".to_string(), fc.clone());
            }
            if let Some(sig) = failure.get("failure_signature") {
                if !sig.is_null() {
                    fields.insert("failure_signature".to_string(), sig.clone());
                }
            }
        }
        // node_id already present from Rust enum
    } else if event_name == "WorkflowRunFailed" {
        // Flatten FabroError to a string for backward compat in progress.jsonl
        if let Some(error_val) = fields.get("error") {
            if error_val.is_object() {
                // Extract the display message from the FabroError serde format
                let display = error_val
                    .get("data")
                    .and_then(|d| {
                        // For struct variants (Handler/Engine): { "data": { "message": "..." } }
                        d.get("message").and_then(|m| m.as_str().map(String::from))
                    })
                    .or_else(|| {
                        // For newtype string variants: { "data": "..." }
                        error_val
                            .get("data")
                            .and_then(|d| d.as_str().map(String::from))
                    })
                    .unwrap_or_else(|| error_val.to_string());
                fields.insert("error".to_string(), serde_json::Value::String(display));
            }
        }
    } else if event_name == "WorkflowRunStarted" {
        rename(fields, "name", "workflow_name");
    } else if event_name.starts_with("Agent.") || event_name == "Agent" {
        rename(fields, "stage", "node_id");
        default_node_label(fields);
    } else if event_name.starts_with("Sandbox.Snapshot") {
        // Must check before generic Sandbox.* to catch Snapshot* first
        rename(fields, "name", "snapshot_name");
        rename(fields, "provider", "sandbox_provider");
    } else if event_name.starts_with("Sandbox.") {
        rename(fields, "provider", "sandbox_provider");
    } else if event_name.starts_with("ParallelBranch") {
        rename(fields, "branch", "node_id");
        default_node_label(fields);
        rename(fields, "index", "branch_index");
    } else if event_name.starts_with("SetupCommand") || event_name == "SetupFailed" {
        rename(fields, "index", "command_index");
    } else if event_name == "EdgeSelected" || event_name == "LoopRestart" {
        rename(fields, "from_node", "from_node_id");
        rename(fields, "to_node", "to_node_id");
    } else if event_name == "StallWatchdogTimeout" {
        rename(fields, "node", "node_id");
        default_node_label(fields);
    } else if event_name == "Prompt" {
        rename(fields, "stage", "node_id");
        default_node_label(fields);
        rename(fields, "text", "prompt_text");
    } else if event_name == "AssetCaptured" {
        default_node_label(fields);
    } else if event_name.starts_with("Interview") && event_name != "InterviewCompleted" {
        // InterviewStarted, InterviewTimeout have `stage`
        rename(fields, "stage", "node_id");
        default_node_label(fields);
    } else if event_name == "SubgraphStarted" {
        default_node_label(fields);
        rename(fields, "start_node", "start_node_id");
    } else if event_name == "SubgraphCompleted"
        || event_name == "CheckpointCompleted"
        || event_name == "CheckpointFailed"
    {
        default_node_label(fields);
    } else if event_name == "GitCommit" {
        if fields.contains_key("node_id") {
            default_node_label(fields);
        }
    } else if event_name.starts_with("DevcontainerLifecycleCommand")
        || event_name == "DevcontainerLifecycleFailed"
    {
        rename(fields, "index", "command_index");
    }
}

/// Current time as epoch milliseconds.
fn epoch_millis() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap()
}

/// Listener callback type for workflow run events.
type EventListener = Arc<dyn Fn(&WorkflowRunEvent) + Send + Sync>;

/// Callback-based event emitter for workflow run events.
pub struct EventEmitter {
    listeners: std::sync::Mutex<Vec<EventListener>>,
    /// Epoch milliseconds of the last `emit()` or `touch()` call. 0 until first event.
    last_event_at: AtomicI64,
}

impl std::fmt::Debug for EventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.listeners.lock().map(|l| l.len()).unwrap_or(0);
        f.debug_struct("EventEmitter")
            .field("listener_count", &count)
            .field("last_event_at", &self.last_event_at.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            listeners: std::sync::Mutex::new(Vec::new()),
            last_event_at: AtomicI64::new(0),
        }
    }

    pub fn on_event(&self, listener: impl Fn(&WorkflowRunEvent) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .expect("listeners lock poisoned")
            .push(Arc::new(listener));
    }

    pub fn emit(&self, event: &WorkflowRunEvent) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
        event.trace();
        // Clone the listener list so we don't hold the lock during dispatch.
        // This prevents deadlocks if a listener calls emit() reentrantly.
        // Note: listeners added during this emit() won't receive the current event.
        let snapshot: Vec<EventListener> = self
            .listeners
            .lock()
            .expect("listeners lock poisoned")
            .clone();
        for listener in &snapshot {
            listener(event);
        }
    }

    /// Returns the epoch milliseconds of the last `emit()` or `touch()` call.
    /// Returns 0 if neither has been called.
    pub fn last_event_at(&self) -> i64 {
        self.last_event_at.load(Ordering::Relaxed)
    }

    /// Manually update the last-event timestamp (e.g. to seed the watchdog at workflow run start).
    pub fn touch(&self) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
    }

    /// Build a [`WorktreeEventCallback`] that forwards worktree lifecycle events as
    /// [`WorkflowRunEvent`]s on this emitter.
    pub fn worktree_callback(self: Arc<Self>) -> WorktreeEventCallback {
        Arc::new(move |event| match event {
            WorktreeEvent::BranchCreated { branch, sha } => {
                self.emit(&WorkflowRunEvent::GitBranch { branch, sha });
            }
            WorktreeEvent::WorktreeAdded { path, branch } => {
                self.emit(&WorkflowRunEvent::GitWorktreeAdd { path, branch });
            }
            WorktreeEvent::WorktreeRemoved { path } => {
                self.emit(&WorkflowRunEvent::GitWorktreeRemove { path });
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_llm::types::Usage;
    use fabro_types::fixtures;
    use std::sync::{Arc, Mutex};

    #[test]
    fn event_emitter_new_has_no_listeners() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }

    #[test]
    fn event_emitter_calls_listener() {
        let emitter = EventEmitter::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        emitter.on_event(move |event| {
            let name = match event {
                WorkflowRunEvent::WorkflowRunStarted { name, .. } => name.clone(),
                _ => "other".to_string(),
            };
            received_clone.lock().unwrap().push(name);
        });
        emitter.emit(&WorkflowRunEvent::WorkflowRunStarted {
            name: "test".to_string(),
            run_id: fixtures::RUN_1,
            base_branch: None,
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
            goal: None,
        });
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], "test");
    }

    #[test]
    fn workflow_run_event_serialization() {
        let event = WorkflowRunEvent::StageStarted {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            handler_type: Some("agent".to_string()),
            script: None,
            attempt: 1,
            max_attempts: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StageStarted"));
        assert!(json.contains("plan"));
        assert!(json.contains("\"handler_type\":\"agent\""));
        assert!(json.contains("\"attempt\":1"));
        assert!(json.contains("\"max_attempts\":3"));

        // None handler_type serializes as null
        let event_none = WorkflowRunEvent::StageStarted {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            handler_type: None,
            script: None,
            attempt: 1,
            max_attempts: 1,
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"handler_type\":null"));
    }

    #[test]
    fn event_emitter_default() {
        let emitter = EventEmitter::default();
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }

    #[test]
    fn agent_event_wrapper_serialization() {
        let event = WorkflowRunEvent::Agent {
            stage: "plan".to_string(),
            event: AgentEvent::ToolCallStarted {
                tool_name: "read_file".to_string(),
                tool_call_id: "call_1".to_string(),
                arguments: serde_json::json!({"path": "/tmp/test.txt"}),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Agent"));
        assert!(json.contains("ToolCallStarted"));
        assert!(json.contains("read_file"));
        assert!(json.contains("plan"));

        // Verify round-trip
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::Agent { stage, .. } if stage == "plan"));
    }

    #[test]
    fn agent_assistant_message_serialization() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::AssistantMessage {
                text: "Here is the implementation".to_string(),
                model: "claude-opus-4-6".to_string(),
                usage: Usage {
                    input_tokens: 1000,
                    output_tokens: 500,
                    total_tokens: 1500,
                    cache_read_tokens: Some(800),
                    cache_write_tokens: Some(50),
                    reasoning_tokens: Some(100),
                    speed: None,
                    raw: None,
                },
                tool_call_count: 3,
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("AssistantMessage"));
        assert!(json.contains("claude-opus-4-6"));
        assert!(json.contains("\"cache_read_tokens\":800"));
        assert!(json.contains("\"reasoning_tokens\":100"));

        // Round-trip
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            WorkflowRunEvent::Agent {
                event: AgentEvent::AssistantMessage { usage, .. },
                ..
            } => {
                assert_eq!(usage.cache_read_tokens, Some(800));
                assert_eq!(usage.reasoning_tokens, Some(100));
            }
            _ => panic!("expected Agent(AssistantMessage)"),
        }
    }

    #[test]
    fn agent_assistant_message_without_cache_tokens_omits_them() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::AssistantMessage {
                text: "response".to_string(),
                model: "test-model".to_string(),
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                    ..Default::default()
                },
                tool_call_count: 0,
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("cache_read_tokens"));
        assert!(!json.contains("reasoning_tokens"));
    }

    #[test]
    fn agent_assistant_output_replace_serialization() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::AssistantOutputReplace {
                text: String::new(),
                reasoning: None,
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("AssistantOutputReplace"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            WorkflowRunEvent::Agent {
                stage,
                event: AgentEvent::AssistantOutputReplace { text, reasoning },
            } => {
                assert_eq!(stage, "code");
                assert!(text.is_empty());
                assert_eq!(reasoning, None);
            }
            _ => panic!("expected Agent(AssistantOutputReplace)"),
        }
    }

    #[test]
    fn stage_completed_event_serialization_with_new_fields() {
        use crate::outcome::{FailureCategory, FailureDetail};

        let event = WorkflowRunEvent::StageCompleted {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            duration_ms: 1500,
            status: "partial_success".to_string(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure: Some(FailureDetail::new(
                "lint errors remain",
                FailureCategory::Deterministic,
            )),
            notes: Some("fixed 3 of 5 issues".to_string()),
            files_touched: vec!["src/main.rs".to_string()],
            attempt: 2,
            max_attempts: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("lint errors remain"));
        assert!(json.contains("\"notes\":\"fixed 3 of 5 issues\""));
        assert!(json.contains("src/main.rs"));
        assert!(json.contains("\"attempt\":2"));
        assert!(json.contains("\"max_attempts\":3"));

        let event_none = WorkflowRunEvent::StageCompleted {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            duration_ms: 1500,
            status: "success".to_string(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure: None,
            notes: None,
            files_touched: vec![],
            attempt: 1,
            max_attempts: 1,
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"notes\":null"));
    }

    #[test]
    fn stage_failed_event_serialization() {
        use crate::outcome::{FailureCategory, FailureDetail};

        let event = WorkflowRunEvent::StageFailed {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            failure: FailureDetail {
                message: "LLM request timed out".to_string(),
                category: FailureCategory::TransientInfra,
                signature: None,
            },
            will_retry: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("LLM request timed out"));
        assert!(json.contains("transient_infra"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::StageFailed { failure, .. } if failure.category == FailureCategory::TransientInfra
        ));

        let event_terminal = WorkflowRunEvent::StageFailed {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            failure: FailureDetail::new("timeout", FailureCategory::Deterministic),
            will_retry: false,
        };
        let json_terminal = serde_json::to_string(&event_terminal).unwrap();
        assert!(json_terminal.contains("deterministic"));
    }

    #[test]
    fn parallel_branch_completed_event_serialization() {
        let event = WorkflowRunEvent::ParallelBranchCompleted {
            branch: "branch_a".to_string(),
            index: 0,
            duration_ms: 1500,
            status: "success".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(!json.contains("\"success\":"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::ParallelBranchCompleted { status, .. } if status == "success")
        );
    }

    #[test]
    fn parallel_started_event_serialization() {
        let event = WorkflowRunEvent::ParallelStarted {
            branch_count: 3,
            join_policy: "wait_all".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"join_policy\":\"wait_all\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::ParallelStarted { join_policy, .. } if join_policy == "wait_all")
        );
    }

    #[test]
    fn interview_started_event_serialization() {
        let event = WorkflowRunEvent::InterviewStarted {
            question: "Review changes?".to_string(),
            stage: "gate".to_string(),
            question_type: "multiple_choice".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"question_type\":\"multiple_choice\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::InterviewStarted { question_type, .. } if question_type == "multiple_choice")
        );
    }

    #[test]
    fn agent_compaction_event_serialization() {
        let started = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::CompactionStarted {
                estimated_tokens: 5000,
                context_window_size: 8000,
            },
        };
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains("CompactionStarted"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::Agent { stage, .. } if stage == "code"));

        let completed = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::CompactionCompleted {
                original_turn_count: 20,
                preserved_turn_count: 6,
                summary_token_estimate: 500,
                tracked_file_count: 3,
            },
        };
        let json = serde_json::to_string(&completed).unwrap();
        assert!(json.contains("CompactionCompleted"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::Agent { stage, .. } if stage == "code"));
    }

    #[test]
    fn edge_selected_event_serialization() {
        let event = WorkflowRunEvent::EdgeSelected {
            from_node: "plan".to_string(),
            to_node: "code".to_string(),
            label: Some("success".to_string()),
            condition: Some("outcome == 'success'".to_string()),
            reason: "condition".to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            stage_status: "success".to_string(),
            is_jump: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("EdgeSelected"));
        assert!(json.contains("\"from_node\":\"plan\""));
        assert!(json.contains("\"to_node\":\"code\""));
        assert!(json.contains("\"label\":\"success\""));
        assert!(json.contains("\"condition\":\"outcome == 'success'\""));
        assert!(json.contains("\"reason\":\"condition\""));
        assert!(json.contains("\"stage_status\":\"success\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::EdgeSelected { from_node, to_node, .. } if from_node == "plan" && to_node == "code")
        );

        // None label/condition
        let event_none = WorkflowRunEvent::EdgeSelected {
            from_node: "a".to_string(),
            to_node: "b".to_string(),
            label: None,
            condition: None,
            reason: "unconditional".to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            stage_status: "success".to_string(),
            is_jump: false,
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"label\":null"));
        assert!(json_none.contains("\"condition\":null"));
    }

    #[test]
    fn loop_restart_event_serialization() {
        let event = WorkflowRunEvent::LoopRestart {
            from_node: "review".to_string(),
            to_node: "code".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("LoopRestart"));
        assert!(json.contains("\"from_node\":\"review\""));
        assert!(json.contains("\"to_node\":\"code\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::LoopRestart { from_node, to_node } if from_node == "review" && to_node == "code")
        );
    }

    #[test]
    fn stage_retrying_event_serialization() {
        let event = WorkflowRunEvent::StageRetrying {
            node_id: "lint".to_string(),
            name: "lint".to_string(),
            index: 2,
            attempt: 3,
            max_attempts: 5,
            delay_ms: 400,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StageRetrying"));
        assert!(json.contains("\"attempt\":3"));
        assert!(json.contains("\"max_attempts\":5"));
        assert!(json.contains("\"delay_ms\":400"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::StageRetrying {
                max_attempts: 5,
                ..
            }
        ));
    }

    #[test]
    fn agent_llm_retry_event_serialization() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::LlmRetry {
                provider: "anthropic".to_string(),
                model: "claude-opus-4-6".to_string(),
                attempt: 2,
                delay_secs: 1.5,
                error: fabro_llm::error::SdkError::Network {
                    message: "rate limited".to_string(),
                    source: None,
                },
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("LlmRetry"));
        assert!(json.contains("\"provider\":\"anthropic\""));
        assert!(json.contains("\"delay_secs\":1.5"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::Agent { stage, .. } if stage == "code"));
    }

    #[test]
    fn subgraph_started_event_serialization() {
        let event = WorkflowRunEvent::SubgraphStarted {
            node_id: "sub_1".to_string(),
            start_node: "start".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("SubgraphStarted"));
        assert!(json.contains("\"node_id\":\"sub_1\""));
        assert!(json.contains("\"start_node\":\"start\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::SubgraphStarted { node_id, .. } if node_id == "sub_1")
        );
    }

    #[test]
    fn subgraph_completed_event_serialization() {
        let event = WorkflowRunEvent::SubgraphCompleted {
            node_id: "sub_1".to_string(),
            steps_executed: 5,
            status: "success".to_string(),
            duration_ms: 3200,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("SubgraphCompleted"));
        assert!(json.contains("\"steps_executed\":5"));
        assert!(json.contains("\"duration_ms\":3200"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::SubgraphCompleted {
                steps_executed: 5,
                ..
            }
        ));
    }

    #[test]
    fn sandbox_event_wrapper_serialization() {
        use fabro_agent::SandboxEvent;

        let event = WorkflowRunEvent::Sandbox {
            event: SandboxEvent::Initializing {
                provider: "docker".into(),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Sandbox"));
        assert!(json.contains("Initializing"));
        assert!(json.contains("docker"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::Sandbox { .. }));
    }

    #[test]
    fn emitter_last_event_at_initially_zero() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.last_event_at(), 0);
    }

    #[test]
    fn emitter_last_event_at_updates_after_emit() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.last_event_at(), 0);
        emitter.emit(&WorkflowRunEvent::WorkflowRunStarted {
            name: "test".to_string(),
            run_id: fixtures::RUN_1,
            base_branch: None,
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
            goal: None,
        });
        assert!(emitter.last_event_at() > 0);
    }

    #[test]
    fn emitter_touch_updates_last_event_at() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.last_event_at(), 0);
        emitter.touch();
        assert!(emitter.last_event_at() > 0);
    }

    #[test]
    fn stall_watchdog_timeout_serialization() {
        let event = WorkflowRunEvent::StallWatchdogTimeout {
            node: "work".to_string(),
            idle_seconds: 600,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StallWatchdogTimeout"));
        assert!(json.contains("\"node\":\"work\""));
        assert!(json.contains("\"idle_seconds\":600"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::StallWatchdogTimeout { node, idle_seconds } if node == "work" && idle_seconds == 600)
        );
    }

    #[test]
    fn serde_round_trip_asset_captured() {
        let event = WorkflowRunEvent::AssetCaptured {
            node_id: "work".to_string(),
            attempt: 2,
            node_slug: "work-visit_3".to_string(),
            path: "coverage/lcov.info".to_string(),
            mime: "application/octet-stream".to_string(),
            content_md5: "abc123".to_string(),
            content_sha256: "def456".to_string(),
            bytes: 512,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::AssetCaptured { node_slug, bytes, .. }
                if node_slug == "work-visit_3" && bytes == 512
        ));
    }

    #[test]
    fn flatten_event_simple_variant() {
        let event = WorkflowRunEvent::StageStarted {
            node_id: "plan".to_string(),
            name: "Plan Stage".to_string(),
            index: 0,
            handler_type: Some("agent".to_string()),
            script: None,
            attempt: 1,
            max_attempts: 3,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "StageStarted");
        assert_eq!(fields["node_id"], "plan");
        assert_eq!(fields["node_label"], "Plan Stage");
        assert_eq!(fields["stage_index"], 0);
        assert_eq!(fields["handler_type"], "agent");
        assert_eq!(fields["attempt"], 1);
        assert_eq!(fields["max_attempts"], 3);
        // Old keys should not be present
        assert!(!fields.contains_key("name"));
        assert!(!fields.contains_key("index"));
    }

    #[test]
    fn flatten_event_agent_tool_call_started() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::ToolCallStarted {
                tool_name: "read_file".to_string(),
                tool_call_id: "call_1".to_string(),
                arguments: serde_json::json!({"path": "/tmp/test.txt"}),
            },
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Agent.ToolCallStarted");
        assert_eq!(fields["node_id"], "code");
        assert_eq!(fields["node_label"], "code");
        assert_eq!(fields["tool_name"], "read_file");
        assert_eq!(fields["tool_call_id"], "call_1");
        assert!(!fields.contains_key("stage"));
    }

    #[test]
    fn flatten_event_sandbox_initializing() {
        let event = WorkflowRunEvent::Sandbox {
            event: SandboxEvent::Initializing {
                provider: "docker".into(),
            },
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Sandbox.Initializing");
        assert_eq!(fields["sandbox_provider"], "docker");
        assert!(!fields.contains_key("provider"));
    }

    #[test]
    fn flatten_event_agent_sub_agent_event() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::SubAgentEvent {
                agent_id: "sub_1".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::ToolCallStarted {
                    tool_name: "write_file".to_string(),
                    tool_call_id: "call_2".to_string(),
                    arguments: serde_json::json!({}),
                }),
            },
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Agent.SubAgentEvent.ToolCallStarted");
        assert_eq!(fields["node_id"], "code");
        assert_eq!(fields["node_label"], "code");
        assert_eq!(fields["agent_id"], "sub_1");
        assert_eq!(fields["depth"], 1);
        assert!(!fields.contains_key("stage"));
        // Inner event preserved as nested_event JSON (not flattened)
        let nested = fields["nested_event"].as_object().unwrap();
        let tool_call = nested["ToolCallStarted"].as_object().unwrap();
        assert_eq!(tool_call["tool_name"], "write_file");
    }

    #[test]
    fn flatten_event_doubly_nested_sub_agent_preserves_all_data() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::SubAgentEvent {
                agent_id: "sub_1".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::SubAgentEvent {
                    agent_id: "sub_2".to_string(),
                    depth: 2,
                    event: Box::new(AgentEvent::ToolCallStarted {
                        tool_name: "read_file".to_string(),
                        tool_call_id: "call_3".to_string(),
                        arguments: serde_json::json!({}),
                    }),
                }),
            },
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Agent.SubAgentEvent.SubAgentEvent");
        // Outer SubAgentEvent fields at top level
        assert_eq!(fields["agent_id"], "sub_1");
        assert_eq!(fields["depth"], 1);
        assert_eq!(fields["node_id"], "code");
        assert_eq!(fields["node_label"], "code");
        assert!(!fields.contains_key("stage"));
        // Inner SubAgentEvent preserved in nested_event with all data intact
        let nested = fields["nested_event"].as_object().unwrap();
        let inner_sub = nested["SubAgentEvent"].as_object().unwrap();
        assert_eq!(inner_sub["agent_id"], "sub_2");
        assert_eq!(inner_sub["depth"], 2);
        let inner_event = inner_sub["event"].as_object().unwrap();
        let tool_call = inner_event["ToolCallStarted"].as_object().unwrap();
        assert_eq!(tool_call["tool_name"], "read_file");
    }

    #[test]
    fn flatten_event_agent_session_started() {
        let event = WorkflowRunEvent::Agent {
            stage: "plan".to_string(),
            event: AgentEvent::SessionStarted,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Agent.SessionStarted");
        assert_eq!(fields["node_id"], "plan");
        assert_eq!(fields["node_label"], "plan");
        assert!(!fields.contains_key("stage"));
    }

    #[test]
    fn flatten_event_agent_assistant_output_replace() {
        let event = WorkflowRunEvent::Agent {
            stage: "plan".to_string(),
            event: AgentEvent::AssistantOutputReplace {
                text: String::new(),
                reasoning: None,
            },
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Agent.AssistantOutputReplace");
        assert_eq!(fields["node_id"], "plan");
        assert_eq!(fields["node_label"], "plan");
        assert_eq!(fields["text"], "");
        assert!(!fields.contains_key("stage"));
    }

    #[test]
    fn rename_fields_workflow_run_started() {
        let event = WorkflowRunEvent::WorkflowRunStarted {
            name: "my_pipeline".to_string(),
            run_id: fixtures::RUN_1,
            base_branch: None,
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
            goal: None,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "WorkflowRunStarted");
        assert_eq!(fields["workflow_name"], "my_pipeline");
        assert!(!fields.contains_key("name"));
    }

    #[test]
    fn rename_fields_parallel_branch_started() {
        let event = WorkflowRunEvent::ParallelBranchStarted {
            branch: "lint".to_string(),
            index: 0,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "ParallelBranchStarted");
        assert_eq!(fields["node_id"], "lint");
        assert_eq!(fields["node_label"], "lint");
        assert_eq!(fields["branch_index"], 0);
        assert!(!fields.contains_key("branch"));
        assert!(!fields.contains_key("index"));
    }

    #[test]
    fn rename_fields_parallel_branch_completed() {
        let event = WorkflowRunEvent::ParallelBranchCompleted {
            branch: "lint".to_string(),
            index: 0,
            duration_ms: 1000,
            status: "success".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "ParallelBranchCompleted");
        assert_eq!(fields["node_id"], "lint");
        assert_eq!(fields["node_label"], "lint");
        assert_eq!(fields["branch_index"], 0);
    }

    #[test]
    fn rename_fields_setup_command_started() {
        let event = WorkflowRunEvent::SetupCommandStarted {
            command: "npm install".to_string(),
            index: 2,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "SetupCommandStarted");
        assert_eq!(fields["command_index"], 2);
        assert!(!fields.contains_key("index"));
    }

    #[test]
    fn rename_fields_setup_failed() {
        let event = WorkflowRunEvent::SetupFailed {
            command: "npm test".to_string(),
            index: 1,
            exit_code: 1,
            stderr: "fail".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "SetupFailed");
        assert_eq!(fields["command_index"], 1);
        assert!(!fields.contains_key("index"));
    }

    #[test]
    fn rename_fields_edge_selected() {
        let event = WorkflowRunEvent::EdgeSelected {
            from_node: "plan".to_string(),
            to_node: "code".to_string(),
            label: Some("success".to_string()),
            condition: None,
            reason: "preferred_label".to_string(),
            preferred_label: Some("success".to_string()),
            suggested_next_ids: Vec::new(),
            stage_status: "success".to_string(),
            is_jump: false,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "EdgeSelected");
        assert_eq!(fields["from_node_id"], "plan");
        assert_eq!(fields["to_node_id"], "code");
        assert!(!fields.contains_key("from_node"));
        assert!(!fields.contains_key("to_node"));
    }

    #[test]
    fn rename_fields_loop_restart() {
        let event = WorkflowRunEvent::LoopRestart {
            from_node: "review".to_string(),
            to_node: "code".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "LoopRestart");
        assert_eq!(fields["from_node_id"], "review");
        assert_eq!(fields["to_node_id"], "code");
    }

    #[test]
    fn rename_fields_stall_watchdog_timeout() {
        let event = WorkflowRunEvent::StallWatchdogTimeout {
            node: "work".to_string(),
            idle_seconds: 600,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "StallWatchdogTimeout");
        assert_eq!(fields["node_id"], "work");
        assert_eq!(fields["node_label"], "work");
        assert!(!fields.contains_key("node"));
    }

    #[test]
    fn rename_fields_prompt() {
        let event = WorkflowRunEvent::Prompt {
            stage: "gate".to_string(),
            text: "Approve?".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Prompt");
        assert_eq!(fields["node_id"], "gate");
        assert_eq!(fields["node_label"], "gate");
        assert_eq!(fields["prompt_text"], "Approve?");
        assert!(!fields.contains_key("stage"));
        assert!(!fields.contains_key("text"));
    }

    #[test]
    fn rename_fields_asset_captured() {
        let event = WorkflowRunEvent::AssetCaptured {
            node_id: "test_stage".to_string(),
            attempt: 1,
            node_slug: "test_stage".to_string(),
            path: "test-results/report.xml".to_string(),
            mime: "text/xml".to_string(),
            content_md5: "d41d8cd98f00b204e9800998ecf8427e".to_string(),
            content_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .to_string(),
            bytes: 1024,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "AssetCaptured");
        assert_eq!(fields["node_id"], "test_stage");
        assert_eq!(fields["node_label"], "test_stage");
        assert_eq!(fields["path"], "test-results/report.xml");
        assert_eq!(fields["bytes"], 1024);
    }

    #[test]
    fn rename_fields_interview_started() {
        let event = WorkflowRunEvent::InterviewStarted {
            question: "OK?".to_string(),
            stage: "gate".to_string(),
            question_type: "yes_no".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "InterviewStarted");
        assert_eq!(fields["node_id"], "gate");
        assert_eq!(fields["node_label"], "gate");
        assert!(!fields.contains_key("stage"));
    }

    #[test]
    fn rename_fields_subgraph_started() {
        let event = WorkflowRunEvent::SubgraphStarted {
            node_id: "sub_1".to_string(),
            start_node: "start".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "SubgraphStarted");
        assert_eq!(fields["node_id"], "sub_1");
        assert_eq!(fields["node_label"], "sub_1");
        assert_eq!(fields["start_node_id"], "start");
        assert!(!fields.contains_key("start_node"));
    }

    #[test]
    fn rename_fields_checkpoint_completed() {
        let event = WorkflowRunEvent::CheckpointCompleted {
            node_id: "work".to_string(),
            status: "success".to_string(),
            git_commit_sha: Some("abc123".to_string()),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "CheckpointCompleted");
        assert_eq!(fields["node_id"], "work");
        assert_eq!(fields["node_label"], "work");

        // Without git_commit_sha
        let event_no_git = WorkflowRunEvent::CheckpointCompleted {
            node_id: "plan".to_string(),
            status: "success".to_string(),
            git_commit_sha: None,
        };
        let json = serde_json::to_string(&event_no_git).unwrap();
        assert!(!json.contains("git_commit_sha"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::CheckpointCompleted {
                git_commit_sha: None,
                ..
            }
        ));
    }

    #[test]
    fn rename_fields_checkpoint_failed() {
        let event = WorkflowRunEvent::CheckpointFailed {
            node_id: "fix_lints".to_string(),
            error: "git add failed (exit 1): fatal: not a git repository".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "CheckpointFailed");
        assert_eq!(fields["node_id"], "fix_lints");
        assert_eq!(fields["node_label"], "fix_lints");
        assert_eq!(
            fields["error"],
            "git add failed (exit 1): fatal: not a git repository"
        );
    }

    #[test]
    fn rename_fields_git_commit_with_node_id() {
        let event = WorkflowRunEvent::GitCommit {
            node_id: Some("work".to_string()),
            sha: "abc123".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "GitCommit");
        assert_eq!(fields["node_id"], "work");
        assert_eq!(fields["node_label"], "work");
        assert_eq!(fields["sha"], "abc123");
    }

    #[test]
    fn rename_fields_git_commit_without_node_id() {
        let event = WorkflowRunEvent::GitCommit {
            node_id: None,
            sha: "abc123".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "GitCommit");
        assert!(!fields.contains_key("node_label"));
        assert_eq!(fields["sha"], "abc123");
    }

    #[test]
    fn git_commit_serialization() {
        let event = WorkflowRunEvent::GitCommit {
            node_id: Some("work".to_string()),
            sha: "abc123".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GitCommit"));
        assert!(json.contains("\"sha\":\"abc123\""));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::GitCommit { sha, .. } if sha == "abc123"));

        // node_id None is omitted
        let event_none = WorkflowRunEvent::GitCommit {
            node_id: None,
            sha: "def456".to_string(),
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(!json_none.contains("node_id"));
    }

    #[test]
    fn git_push_serialization() {
        let event = WorkflowRunEvent::GitPush {
            branch: "fabro/run/123".to_string(),
            success: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GitPush"));
        assert!(json.contains("\"success\":true"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::GitPush { success: true, .. }
        ));
    }

    #[test]
    fn git_branch_serialization() {
        let event = WorkflowRunEvent::GitBranch {
            branch: "fabro/run/123/work".to_string(),
            sha: "abc123".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GitBranch"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::GitBranch { branch, .. } if branch == "fabro/run/123/work")
        );
    }

    #[test]
    fn git_worktree_add_serialization() {
        let event = WorkflowRunEvent::GitWorktreeAdd {
            path: "/tmp/wt".to_string(),
            branch: "work".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GitWorktreeAdd"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::GitWorktreeAdd { path, .. } if path == "/tmp/wt")
        );
    }

    #[test]
    fn git_worktree_remove_serialization() {
        let event = WorkflowRunEvent::GitWorktreeRemove {
            path: "/tmp/wt".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GitWorktreeRemove"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::GitWorktreeRemove { path } if path == "/tmp/wt")
        );
    }

    #[test]
    fn git_fetch_serialization() {
        let event = WorkflowRunEvent::GitFetch {
            branch: "main".to_string(),
            success: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GitFetch"));
        assert!(json.contains("\"success\":false"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::GitFetch { success: false, .. }
        ));
    }

    #[test]
    fn git_reset_serialization() {
        let event = WorkflowRunEvent::GitReset {
            sha: "abc123".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GitReset"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::GitReset { sha } if sha == "abc123"));
    }

    #[test]
    fn rename_fields_sandbox_snapshot_pulling() {
        let event = WorkflowRunEvent::Sandbox {
            event: SandboxEvent::SnapshotPulling {
                name: "base-image".into(),
            },
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "Sandbox.SnapshotPulling");
        assert_eq!(fields["snapshot_name"], "base-image");
        assert!(!fields.contains_key("name"));
    }

    #[test]
    fn cli_ensure_events_serialization() {
        let events = vec![
            WorkflowRunEvent::CliEnsureStarted {
                cli_name: "claude".into(),
                provider: "anthropic".into(),
            },
            WorkflowRunEvent::CliEnsureCompleted {
                cli_name: "claude".into(),
                provider: "anthropic".into(),
                already_installed: false,
                node_installed: true,
                duration_ms: 45000,
            },
            WorkflowRunEvent::CliEnsureFailed {
                cli_name: "codex".into(),
                provider: "openai".into(),
                error: "npm install failed".into(),
                duration_ms: 30000,
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn setup_events_serialization() {
        let events = vec![
            WorkflowRunEvent::SetupStarted { command_count: 3 },
            WorkflowRunEvent::SetupCommandStarted {
                command: "npm install".into(),
                index: 0,
            },
            WorkflowRunEvent::SetupCommandCompleted {
                command: "npm install".into(),
                index: 0,
                exit_code: 0,
                duration_ms: 5000,
            },
            WorkflowRunEvent::SetupCompleted { duration_ms: 8000 },
            WorkflowRunEvent::SetupFailed {
                command: "npm test".into(),
                index: 1,
                exit_code: 1,
                stderr: "test failed".into(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn pull_request_created_event_serialization() {
        let event = WorkflowRunEvent::PullRequestCreated {
            pr_url: "https://github.com/owner/repo/pull/42".to_string(),
            pr_number: 42,
            draft: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("PullRequestCreated"));
        assert!(json.contains("\"pr_number\":42"));
        assert!(json.contains("\"draft\":true"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::PullRequestCreated {
                pr_number: 42,
                draft: true,
                ..
            }
        ));
    }

    #[test]
    fn pull_request_failed_event_serialization() {
        let event = WorkflowRunEvent::PullRequestFailed {
            error: "auth failed".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("PullRequestFailed"));
        assert!(json.contains("\"error\":\"auth failed\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::PullRequestFailed { error } if error == "auth failed")
        );
    }

    #[test]
    fn run_notice_event_serialization() {
        let event = WorkflowRunEvent::RunNotice {
            level: RunNoticeLevel::Warn,
            code: "sandbox_cleanup_failed".to_string(),
            message: "sandbox cleanup failed: boom".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("RunNotice"));
        assert!(json.contains("\"level\":\"warn\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code,
                ..
            } if code == "sandbox_cleanup_failed"
        ));
    }

    #[test]
    fn flatten_event_run_notice() {
        let event = WorkflowRunEvent::RunNotice {
            level: RunNoticeLevel::Error,
            code: "bootstrap_failed".to_string(),
            message: "working directory missing".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "RunNotice");
        assert_eq!(fields.get("level").and_then(|v| v.as_str()), Some("error"));
        assert_eq!(
            fields.get("code").and_then(|v| v.as_str()),
            Some("bootstrap_failed")
        );
    }

    #[test]
    fn workflow_run_completed_serialization_with_status_and_usage() {
        let event = WorkflowRunEvent::WorkflowRunCompleted {
            duration_ms: 30000,
            artifact_count: 2,
            status: "success".to_string(),
            total_cost: Some(1.23),
            final_git_commit_sha: Some("abc123".to_string()),
            usage: Some(Usage {
                input_tokens: 5000,
                output_tokens: 2000,
                total_tokens: 7000,
                cache_read_tokens: Some(3000),
                cache_write_tokens: Some(500),
                reasoning_tokens: Some(800),
                speed: None,
                raw: None,
            }),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(json.contains("\"total_tokens\":7000"));
        assert!(json.contains("\"cache_read_tokens\":3000"));
        assert!(json.contains("\"reasoning_tokens\":800"));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::WorkflowRunCompleted { status, usage: Some(u), .. }
                if status == "success" && u.total_tokens == 7000
        ));
    }

    #[test]
    fn workflow_run_completed_backward_compat_without_new_fields() {
        // Old JSONL without status/usage should deserialize with defaults
        let json =
            r#"{"WorkflowRunCompleted":{"duration_ms":5000,"artifact_count":1,"total_cost":0.25}}"#;
        let deserialized: WorkflowRunEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::WorkflowRunCompleted { status, usage, .. }
                if status.is_empty() && usage.is_none()
        ));
    }

    #[test]
    fn devcontainer_resolved_serializes() {
        let event = WorkflowRunEvent::DevcontainerResolved {
            dockerfile_lines: 15,
            environment_count: 3,
            lifecycle_command_count: 5,
            workspace_folder: "/workspaces/myrepo".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("DevcontainerResolved"));
        assert!(json.contains("\"dockerfile_lines\":15"));
        assert!(json.contains("\"workspace_folder\":\"/workspaces/myrepo\""));

        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::DevcontainerResolved {
                dockerfile_lines: 15,
                ..
            }
        ));
    }

    #[test]
    fn devcontainer_lifecycle_events_serialization() {
        let events = vec![
            WorkflowRunEvent::DevcontainerLifecycleStarted {
                phase: "on_create".into(),
                command_count: 2,
            },
            WorkflowRunEvent::DevcontainerLifecycleCommandStarted {
                phase: "on_create".into(),
                command: "npm install".into(),
                index: 0,
            },
            WorkflowRunEvent::DevcontainerLifecycleCommandCompleted {
                phase: "on_create".into(),
                command: "npm install".into(),
                index: 0,
                exit_code: 0,
                duration_ms: 5000,
            },
            WorkflowRunEvent::DevcontainerLifecycleCompleted {
                phase: "on_create".into(),
                duration_ms: 8000,
            },
            WorkflowRunEvent::DevcontainerLifecycleFailed {
                phase: "post_create".into(),
                command: "npm test".into(),
                index: 1,
                exit_code: 1,
                stderr: "test failed".into(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn flatten_devcontainer_lifecycle_command_renames_index() {
        let event = WorkflowRunEvent::DevcontainerLifecycleCommandStarted {
            phase: "on_create".into(),
            command: "npm install".into(),
            index: 2,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "DevcontainerLifecycleCommandStarted");
        assert_eq!(fields["command_index"], 2);
        assert!(!fields.contains_key("index"));
    }

    #[test]
    fn flatten_devcontainer_lifecycle_failed_renames_index() {
        let event = WorkflowRunEvent::DevcontainerLifecycleFailed {
            phase: "post_create".into(),
            command: "npm test".into(),
            index: 1,
            exit_code: 1,
            stderr: "fail".into(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "DevcontainerLifecycleFailed");
        assert_eq!(fields["command_index"], 1);
        assert!(!fields.contains_key("index"));
    }

    #[test]
    fn workflow_run_started_with_goal_round_trip() {
        let event = WorkflowRunEvent::WorkflowRunStarted {
            name: "my_workflow".to_string(),
            run_id: fixtures::RUN_42,
            base_branch: None,
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
            goal: Some("Fix the bug".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"goal\":\"Fix the bug\""));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, WorkflowRunEvent::WorkflowRunStarted { goal: Some(g), .. } if g == "Fix the bug")
        );
    }

    #[test]
    fn workflow_run_started_without_goal_backward_compat() {
        // Old JSONL without `goal` field should deserialize to `goal: None`
        let json =
            r#"{"WorkflowRunStarted":{"name":"old_wf","run_id":"00000000000000000000000001"}}"#;
        let deserialized: WorkflowRunEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::WorkflowRunStarted { goal: None, .. }
        ));
    }

    #[test]
    fn workflow_run_started_goal_none_omitted_from_json() {
        let event = WorkflowRunEvent::WorkflowRunStarted {
            name: "wf".to_string(),
            run_id: fixtures::RUN_1,
            base_branch: None,
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
            goal: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            !json.contains("goal"),
            "goal: None should be skipped, got: {json}"
        );
    }

    #[test]
    fn retro_started_event_serialization() {
        let event = WorkflowRunEvent::RetroStarted;
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("RetroStarted"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::RetroStarted));
    }

    #[test]
    fn retro_completed_event_serialization() {
        let event = WorkflowRunEvent::RetroCompleted { duration_ms: 5000 };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"duration_ms\":5000"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::RetroCompleted { duration_ms: 5000 }
        ));
    }

    #[test]
    fn retro_failed_event_serialization() {
        let event = WorkflowRunEvent::RetroFailed {
            error: "LLM timeout".to_string(),
            duration_ms: 3000,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("LLM timeout"));
        assert!(json.contains("\"duration_ms\":3000"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, WorkflowRunEvent::RetroFailed { .. }));
    }

    #[test]
    fn flatten_retro_started() {
        let event = WorkflowRunEvent::RetroStarted;
        let (name, _fields) = flatten_event(&event);
        assert_eq!(name, "RetroStarted");
    }

    #[test]
    fn flatten_retro_failed() {
        let event = WorkflowRunEvent::RetroFailed {
            error: "timeout".to_string(),
            duration_ms: 1000,
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "RetroFailed");
        assert_eq!(fields["error"], "timeout");
        assert_eq!(fields["duration_ms"], 1000);
    }

    #[test]
    fn emitter_captures_retro_events() {
        let emitter = EventEmitter::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let r = Arc::clone(&received);
        emitter.on_event(move |event| {
            if let WorkflowRunEvent::RetroStarted = event {
                r.lock().unwrap().push("started".to_string());
            }
            if let WorkflowRunEvent::RetroCompleted { .. } = event {
                r.lock().unwrap().push("completed".to_string());
            }
        });
        emitter.emit(&WorkflowRunEvent::RetroStarted);
        emitter.emit(&WorkflowRunEvent::RetroCompleted { duration_ms: 100 });
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], "started");
        assert_eq!(events[1], "completed");
    }

    #[test]
    fn sandbox_initialized_event_serialization() {
        let event = WorkflowRunEvent::SandboxInitialized {
            working_directory: "/workspace/project".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("SandboxInitialized"));
        assert!(json.contains("/workspace/project"));
        let deserialized: WorkflowRunEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            WorkflowRunEvent::SandboxInitialized {
                working_directory
            } if working_directory == "/workspace/project"
        ));
    }

    #[test]
    fn flatten_sandbox_initialized() {
        let event = WorkflowRunEvent::SandboxInitialized {
            working_directory: "/workspace".to_string(),
        };
        let (name, fields) = flatten_event(&event);
        assert_eq!(name, "SandboxInitialized");
        assert_eq!(fields["working_directory"], "/workspace");
    }
}
