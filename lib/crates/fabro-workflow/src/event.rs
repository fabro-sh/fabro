use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use fabro_store::{EventPayload, SlateRunStore};
use fabro_types::{RunId, StoredEvent};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::error::FabroError;
use crate::outcome::{FailureDetail, Outcome, StageUsage};
use fabro_agent::{AgentEvent, SandboxEvent, WorktreeEvent, WorktreeEventCallback};
use fabro_llm::types::Usage as LlmUsage;
use fabro_types::StatusReason;
use fabro_util::redact::redact_jsonl_line;

pub use fabro_types::{EventBody, RunNoticeLevel};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEventEnvelope {
    pub id: String,
    pub ts: String,
    pub run_id: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_label: Option<String>,
    pub properties: serde_json::Value,
}

impl From<&RunEventEnvelope> for StoredEvent {
    fn from(value: &RunEventEnvelope) -> Self {
        StoredEvent::from_value(serde_json::to_value(value).expect("event envelope serializes"))
            .expect("event envelope converts to stored event")
    }
}

/// Events emitted during workflow run execution for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum WorkflowRunEvent {
    RunCreated {
        run_id: RunId,
        settings: serde_json::Value,
        graph: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_config: Option<String>,
        labels: BTreeMap<String, String>,
        run_dir: String,
        working_directory: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_repo_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_slug: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        db_prefix: Option<String>,
    },
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
    RunSubmitted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StatusReason>,
    },
    RunStarting {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StatusReason>,
    },
    RunRunning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StatusReason>,
    },
    RunRemoving {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StatusReason>,
    },
    RunRewound {
        target_checkpoint_ordinal: usize,
        target_node_id: String,
        target_visit: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_commit_sha: Option<String>,
    },
    WorkflowRunCompleted {
        duration_ms: u64,
        artifact_count: usize,
        #[serde(default)]
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StatusReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_cost: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_git_commit_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_patch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<LlmUsage>,
    },
    WorkflowRunFailed {
        error: FabroError,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StatusReason>,
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
        handler_type: String,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_updates: Option<BTreeMap<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jump_to_node: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_values: Option<BTreeMap<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_visits: Option<BTreeMap<String, usize>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_failure_signatures: Option<BTreeMap<String, usize>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restart_failure_signatures: Option<BTreeMap<String, usize>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<String>,
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
        node_id: String,
        visit: u32,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_sha: Option<String>,
    },
    ParallelCompleted {
        node_id: String,
        visit: u32,
        duration_ms: u64,
        success_count: usize,
        failure_count: usize,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        results: Vec<serde_json::Value>,
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
        current_node: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        completed_nodes: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        node_retries: BTreeMap<String, u32>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        context_values: BTreeMap<String, serde_json::Value>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        node_outcomes: BTreeMap<String, Outcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_node_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_commit_sha: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        loop_failure_signatures: BTreeMap<String, usize>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        restart_failure_signatures: BTreeMap<String, usize>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        node_visits: BTreeMap<String, usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
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
        visit: u32,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    PromptCompleted {
        node_id: String,
        response: String,
        model: String,
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<StageUsage>,
    },
    /// Forwarded from an agent session, tagged with the workflow stage.
    Agent {
        stage: String,
        visit: u32,
        event: AgentEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
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
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identifier: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_working_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        container_mount_point: Option<String>,
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
    CommandStarted {
        node_id: String,
        script: String,
        command: String,
        language: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    CommandCompleted {
        node_id: String,
        stdout: String,
        stderr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: u64,
        timed_out: bool,
    },
    AgentCliStarted {
        node_id: String,
        visit: u32,
        mode: String,
        provider: String,
        model: String,
        command: String,
    },
    AgentCliCompleted {
        node_id: String,
        stdout: String,
        stderr: String,
        exit_code: i32,
        duration_ms: u64,
    },
    PullRequestCreated {
        pr_url: String,
        pr_number: u64,
        owner: String,
        repo: String,
        base_branch: String,
        head_branch: String,
        title: String,
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
    RetroStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    RetroCompleted {
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retro: Option<serde_json::Value>,
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
            Self::RunCreated {
                run_id, run_dir, ..
            } => {
                info!(run_id = %run_id, run_dir, "Run created");
            }
            Self::WorkflowRunStarted { name, run_id, .. } => {
                info!(workflow = name.as_str(), run_id = %run_id, "Workflow run started");
            }
            Self::RunSubmitted { reason } => {
                info!(?reason, "Run submitted");
            }
            Self::RunStarting { reason } => {
                info!(?reason, "Run starting");
            }
            Self::RunRunning { reason } => {
                info!(?reason, "Run running");
            }
            Self::RunRemoving { reason } => {
                info!(?reason, "Run removing");
            }
            Self::RunRewound {
                target_checkpoint_ordinal,
                target_node_id,
                target_visit,
                previous_status,
                run_commit_sha,
            } => {
                info!(
                    target_checkpoint_ordinal,
                    target_node_id,
                    target_visit,
                    previous_status = previous_status.as_deref().unwrap_or(""),
                    run_commit_sha = run_commit_sha.as_deref().unwrap_or(""),
                    "Run rewound"
                );
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
                    handler_type,
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
                ..
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
                ..
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
                results,
                ..
            } => {
                debug!(
                    duration_ms,
                    success_count,
                    failure_count,
                    result_count = results.len(),
                    "Parallel execution completed"
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
                node_id,
                status,
                completed_nodes,
                ..
            } => {
                debug!(
                    node_id,
                    status,
                    completed_count = completed_nodes.len(),
                    "Checkpoint completed"
                );
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
            Self::Prompt {
                stage,
                text,
                mode,
                provider,
                model,
                ..
            } => {
                debug!(
                    stage,
                    text_len = text.len(),
                    mode = mode.as_deref().unwrap_or(""),
                    provider = provider.as_deref().unwrap_or(""),
                    model = model.as_deref().unwrap_or(""),
                    "Prompt sent"
                );
            }
            Self::PromptCompleted {
                node_id,
                model,
                provider,
                ..
            } => {
                debug!(node_id, model, provider, "Prompt completed");
            }
            Self::Agent { .. } | Self::Sandbox { .. } => {}
            Self::SandboxInitialized {
                working_directory,
                provider,
                identifier,
                ..
            } => {
                info!(
                    working_directory,
                    provider,
                    identifier = identifier.as_deref().unwrap_or(""),
                    "Sandbox initialized"
                );
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
            Self::CommandStarted {
                node_id,
                language,
                timeout_ms,
                ..
            } => {
                debug!(node_id, language, timeout_ms, "Command started");
            }
            Self::CommandCompleted {
                node_id,
                exit_code,
                duration_ms,
                timed_out,
                ..
            } => {
                debug!(
                    node_id,
                    exit_code, duration_ms, timed_out, "Command completed"
                );
            }
            Self::AgentCliStarted {
                node_id,
                provider,
                model,
                ..
            } => {
                debug!(node_id, provider, model, "Agent CLI started");
            }
            Self::AgentCliCompleted {
                node_id,
                exit_code,
                duration_ms,
                ..
            } => {
                debug!(node_id, exit_code, duration_ms, "Agent CLI completed");
            }
            Self::PullRequestCreated {
                pr_url,
                pr_number,
                draft,
                owner,
                repo,
                ..
            } => {
                info!(pr_url = %pr_url, pr_number, draft, owner, repo, "Pull request created");
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
            Self::RetroStarted {
                prompt: _,
                provider,
                model,
            } => {
                info!(
                    provider = provider.as_deref().unwrap_or(""),
                    model = model.as_deref().unwrap_or(""),
                    "Retro started"
                );
            }
            Self::RetroCompleted { duration_ms, .. } => {
                info!(duration_ms, "Retro completed");
            }
            Self::RetroFailed { error, duration_ms } => {
                error!(error = %error, duration_ms, "Retro failed");
            }
        }
    }
}

pub fn event_name(event: &WorkflowRunEvent) -> &'static str {
    match event {
        WorkflowRunEvent::RunCreated { .. } => "run.created",
        WorkflowRunEvent::WorkflowRunStarted { .. } => "run.started",
        WorkflowRunEvent::RunSubmitted { .. } => "run.submitted",
        WorkflowRunEvent::RunStarting { .. } => "run.starting",
        WorkflowRunEvent::RunRunning { .. } => "run.running",
        WorkflowRunEvent::RunRemoving { .. } => "run.removing",
        WorkflowRunEvent::RunRewound { .. } => "run.rewound",
        WorkflowRunEvent::WorkflowRunCompleted { .. } => "run.completed",
        WorkflowRunEvent::WorkflowRunFailed { .. } => "run.failed",
        WorkflowRunEvent::RunNotice { .. } => "run.notice",
        WorkflowRunEvent::StageStarted { .. } => "stage.started",
        WorkflowRunEvent::StageCompleted { .. } => "stage.completed",
        WorkflowRunEvent::StageFailed { .. } => "stage.failed",
        WorkflowRunEvent::StageRetrying { .. } => "stage.retrying",
        WorkflowRunEvent::ParallelStarted { .. } => "parallel.started",
        WorkflowRunEvent::ParallelBranchStarted { .. } => "parallel.branch.started",
        WorkflowRunEvent::ParallelBranchCompleted { .. } => "parallel.branch.completed",
        WorkflowRunEvent::ParallelCompleted { .. } => "parallel.completed",
        WorkflowRunEvent::InterviewStarted { .. } => "interview.started",
        WorkflowRunEvent::InterviewCompleted { .. } => "interview.completed",
        WorkflowRunEvent::InterviewTimeout { .. } => "interview.timeout",
        WorkflowRunEvent::CheckpointCompleted { .. } => "checkpoint.completed",
        WorkflowRunEvent::CheckpointFailed { .. } => "checkpoint.failed",
        WorkflowRunEvent::GitCommit { .. } => "git.commit",
        WorkflowRunEvent::GitPush { .. } => "git.push",
        WorkflowRunEvent::GitBranch { .. } => "git.branch",
        WorkflowRunEvent::GitWorktreeAdd { .. } => "git.worktree.added",
        WorkflowRunEvent::GitWorktreeRemove { .. } => "git.worktree.removed",
        WorkflowRunEvent::GitFetch { .. } => "git.fetch",
        WorkflowRunEvent::GitReset { .. } => "git.reset",
        WorkflowRunEvent::EdgeSelected { .. } => "edge.selected",
        WorkflowRunEvent::LoopRestart { .. } => "loop.restart",
        WorkflowRunEvent::Prompt { .. } => "stage.prompt",
        WorkflowRunEvent::PromptCompleted { .. } => "prompt.completed",
        WorkflowRunEvent::Agent { event, .. } => match event {
            AgentEvent::SessionStarted { .. } => "agent.session.started",
            AgentEvent::SessionEnded => "agent.session.ended",
            AgentEvent::ProcessingEnd => "agent.processing.end",
            AgentEvent::UserInput { .. } => "agent.input",
            AgentEvent::AssistantTextStart => "agent.output.start",
            AgentEvent::AssistantOutputReplace { .. } => "agent.output.replace",
            AgentEvent::AssistantMessage { .. } => "agent.message",
            AgentEvent::TextDelta { .. } => "agent.text.delta",
            AgentEvent::ReasoningDelta { .. } => "agent.reasoning.delta",
            AgentEvent::ToolCallStarted { .. } => "agent.tool.started",
            AgentEvent::ToolCallOutputDelta { .. } => "agent.tool.output.delta",
            AgentEvent::ToolCallCompleted { .. } => "agent.tool.completed",
            AgentEvent::Error { .. } => "agent.error",
            AgentEvent::Warning { .. } => "agent.warning",
            AgentEvent::LoopDetected => "agent.loop.detected",
            AgentEvent::TurnLimitReached { .. } => "agent.turn.limit",
            AgentEvent::SkillExpanded { .. } => "agent.skill.expanded",
            AgentEvent::SteeringInjected { .. } => "agent.steering.injected",
            AgentEvent::CompactionStarted { .. } => "agent.compaction.started",
            AgentEvent::CompactionCompleted { .. } => "agent.compaction.completed",
            AgentEvent::LlmRetry { .. } => "agent.llm.retry",
            AgentEvent::SubAgentSpawned { .. } => "agent.sub.spawned",
            AgentEvent::SubAgentCompleted { .. } => "agent.sub.completed",
            AgentEvent::SubAgentFailed { .. } => "agent.sub.failed",
            AgentEvent::SubAgentClosed { .. } => "agent.sub.closed",
            AgentEvent::McpServerReady { .. } => "agent.mcp.ready",
            AgentEvent::McpServerFailed { .. } => "agent.mcp.failed",
        },
        WorkflowRunEvent::SubgraphStarted { .. } => "subgraph.started",
        WorkflowRunEvent::SubgraphCompleted { .. } => "subgraph.completed",
        WorkflowRunEvent::Sandbox { event } => match event {
            SandboxEvent::Initializing { .. } => "sandbox.initializing",
            SandboxEvent::Ready { .. } => "sandbox.ready",
            SandboxEvent::InitializeFailed { .. } => "sandbox.failed",
            SandboxEvent::CleanupStarted { .. } => "sandbox.cleanup.started",
            SandboxEvent::CleanupCompleted { .. } => "sandbox.cleanup.completed",
            SandboxEvent::CleanupFailed { .. } => "sandbox.cleanup.failed",
            SandboxEvent::SnapshotPulling { .. } => "sandbox.snapshot.pulling",
            SandboxEvent::SnapshotPulled { .. } => "sandbox.snapshot.pulled",
            SandboxEvent::SnapshotEnsuring { .. } => "sandbox.snapshot.ensuring",
            SandboxEvent::SnapshotCreating { .. } => "sandbox.snapshot.creating",
            SandboxEvent::SnapshotReady { .. } => "sandbox.snapshot.ready",
            SandboxEvent::SnapshotFailed { .. } => "sandbox.snapshot.failed",
            SandboxEvent::GitCloneStarted { .. } => "sandbox.git.started",
            SandboxEvent::GitCloneCompleted { .. } => "sandbox.git.completed",
            SandboxEvent::GitCloneFailed { .. } => "sandbox.git.failed",
        },
        WorkflowRunEvent::SandboxInitialized { .. } => "sandbox.initialized",
        WorkflowRunEvent::SetupStarted { .. } => "setup.started",
        WorkflowRunEvent::SetupCommandStarted { .. } => "setup.command.started",
        WorkflowRunEvent::SetupCommandCompleted { .. } => "setup.command.completed",
        WorkflowRunEvent::SetupCompleted { .. } => "setup.completed",
        WorkflowRunEvent::SetupFailed { .. } => "setup.failed",
        WorkflowRunEvent::StallWatchdogTimeout { .. } => "watchdog.timeout",
        WorkflowRunEvent::AssetCaptured { .. } => "asset.captured",
        WorkflowRunEvent::SshAccessReady { .. } => "ssh.ready",
        WorkflowRunEvent::Failover { .. } => "agent.failover",
        WorkflowRunEvent::CliEnsureStarted { .. } => "cli.ensure.started",
        WorkflowRunEvent::CliEnsureCompleted { .. } => "cli.ensure.completed",
        WorkflowRunEvent::CliEnsureFailed { .. } => "cli.ensure.failed",
        WorkflowRunEvent::CommandStarted { .. } => "command.started",
        WorkflowRunEvent::CommandCompleted { .. } => "command.completed",
        WorkflowRunEvent::AgentCliStarted { .. } => "agent.cli.started",
        WorkflowRunEvent::AgentCliCompleted { .. } => "agent.cli.completed",
        WorkflowRunEvent::PullRequestCreated { .. } => "pull_request.created",
        WorkflowRunEvent::PullRequestFailed { .. } => "pull_request.failed",
        WorkflowRunEvent::DevcontainerResolved { .. } => "devcontainer.resolved",
        WorkflowRunEvent::DevcontainerLifecycleStarted { .. } => "devcontainer.lifecycle.started",
        WorkflowRunEvent::DevcontainerLifecycleCommandStarted { .. } => {
            "devcontainer.lifecycle.command.started"
        }
        WorkflowRunEvent::DevcontainerLifecycleCommandCompleted { .. } => {
            "devcontainer.lifecycle.command.completed"
        }
        WorkflowRunEvent::DevcontainerLifecycleCompleted { .. } => {
            "devcontainer.lifecycle.completed"
        }
        WorkflowRunEvent::DevcontainerLifecycleFailed { .. } => "devcontainer.lifecycle.failed",
        WorkflowRunEvent::RetroStarted { .. } => "retro.started",
        WorkflowRunEvent::RetroCompleted { .. } => "retro.completed",
        WorkflowRunEvent::RetroFailed { .. } => "retro.failed",
    }
}

#[derive(Debug)]
struct EnvelopeFields {
    session_id: Option<String>,
    parent_session_id: Option<String>,
    node_id: Option<String>,
    node_label: Option<String>,
    properties: Value,
}

fn tagged_variant_fields<T: Serialize>(value: &T) -> Map<String, Value> {
    tagged_variant_fields_from_value(serde_json::to_value(value).expect("serializable event"))
}

fn tagged_variant_fields_from_value(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => {
            let (_, inner) = map.into_iter().next().expect("enum must have one variant");
            match inner {
                Value::Object(fields) => fields,
                Value::String(_) | Value::Null => Map::new(),
                other => {
                    let mut fields = Map::new();
                    fields.insert("value".to_string(), other);
                    fields
                }
            }
        }
        Value::String(_) | Value::Null => Map::new(),
        other => {
            let mut fields = Map::new();
            fields.insert("value".to_string(), other);
            fields
        }
    }
}

fn remove_string(fields: &mut Map<String, Value>, key: &str) -> Option<String> {
    match fields.remove(key) {
        Some(Value::String(value)) => Some(value),
        _ => None,
    }
}

fn flatten_failure_detail(fields: &mut Map<String, Value>) {
    let Some(Value::Object(failure)) = fields.remove("failure") else {
        return;
    };
    if let Some(message) = failure.get("message").cloned() {
        fields.insert("error".to_string(), message);
    }
    if let Some(failure_class) = failure.get("failure_class").cloned() {
        fields.insert("failure_class".to_string(), failure_class);
    }
    if let Some(failure_signature) = failure.get("failure_signature").cloned() {
        if !failure_signature.is_null() {
            fields.insert("failure_signature".to_string(), failure_signature);
        }
    }
}

fn default_node_label(node_id: Option<&String>, node_label: Option<String>) -> Option<String> {
    node_label.or_else(|| node_id.cloned())
}

fn extract_envelope_fields(event: &WorkflowRunEvent) -> EnvelopeFields {
    match event {
        WorkflowRunEvent::RunCreated { .. } | WorkflowRunEvent::WorkflowRunStarted { .. } => {
            let mut fields = tagged_variant_fields(event);
            fields.remove("run_id");
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id: None,
                node_label: None,
                properties: Value::Object(fields),
            }
        }
        WorkflowRunEvent::WorkflowRunFailed { error, .. } => {
            let mut fields = tagged_variant_fields(event);
            fields.insert("error".to_string(), Value::String(error.to_string()));
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id: None,
                node_label: None,
                properties: Value::Object(fields),
            }
        }
        WorkflowRunEvent::StageCompleted { .. } | WorkflowRunEvent::StageFailed { .. } => {
            let mut fields = tagged_variant_fields(event);
            let node_id = remove_string(&mut fields, "node_id");
            let node_label =
                default_node_label(node_id.as_ref(), remove_string(&mut fields, "name"));
            flatten_failure_detail(&mut fields);
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id,
                node_label,
                properties: Value::Object(fields),
            }
        }
        WorkflowRunEvent::StageStarted { .. }
        | WorkflowRunEvent::StageRetrying { .. }
        | WorkflowRunEvent::CheckpointCompleted { .. }
        | WorkflowRunEvent::CheckpointFailed { .. }
        | WorkflowRunEvent::SubgraphStarted { .. }
        | WorkflowRunEvent::SubgraphCompleted { .. }
        | WorkflowRunEvent::AssetCaptured { .. }
        | WorkflowRunEvent::PromptCompleted { .. }
        | WorkflowRunEvent::ParallelStarted { .. }
        | WorkflowRunEvent::ParallelCompleted { .. }
        | WorkflowRunEvent::CommandStarted { .. }
        | WorkflowRunEvent::CommandCompleted { .. }
        | WorkflowRunEvent::AgentCliStarted { .. }
        | WorkflowRunEvent::AgentCliCompleted { .. } => {
            let mut fields = tagged_variant_fields(event);
            let node_id = remove_string(&mut fields, "node_id");
            let node_label =
                default_node_label(node_id.as_ref(), remove_string(&mut fields, "name"));
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id,
                node_label,
                properties: Value::Object(fields),
            }
        }
        WorkflowRunEvent::Agent {
            session_id,
            parent_session_id,
            ..
        } => {
            let mut fields = tagged_variant_fields(event);
            let node_id = remove_string(&mut fields, "stage");
            let node_label = default_node_label(node_id.as_ref(), None);
            let visit = fields.remove("visit");
            fields.remove("session_id");
            fields.remove("parent_session_id");
            let mut properties = fields.remove("event").map_or_else(
                || Value::Object(Map::new()),
                |value| Value::Object(tagged_variant_fields_from_value(value)),
            );
            if let (Some(visit), Value::Object(map)) = (visit, &mut properties) {
                map.insert("visit".to_string(), visit);
            }
            EnvelopeFields {
                session_id: session_id.clone(),
                parent_session_id: parent_session_id.clone(),
                node_id,
                node_label,
                properties,
            }
        }
        WorkflowRunEvent::Sandbox { .. } => {
            let mut fields = tagged_variant_fields(event);
            let properties = fields.remove("event").map_or_else(
                || Value::Object(Map::new()),
                |value| Value::Object(tagged_variant_fields_from_value(value)),
            );
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id: None,
                node_label: None,
                properties,
            }
        }
        WorkflowRunEvent::GitCommit { .. } => {
            let mut fields = tagged_variant_fields(event);
            let node_id = remove_string(&mut fields, "node_id");
            let node_label = default_node_label(node_id.as_ref(), None);
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id,
                node_label,
                properties: Value::Object(fields),
            }
        }
        WorkflowRunEvent::ParallelBranchStarted { .. }
        | WorkflowRunEvent::ParallelBranchCompleted { .. } => {
            let mut fields = tagged_variant_fields(event);
            let node_id = remove_string(&mut fields, "branch");
            let node_label = default_node_label(node_id.as_ref(), None);
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id,
                node_label,
                properties: Value::Object(fields),
            }
        }
        WorkflowRunEvent::Prompt { .. }
        | WorkflowRunEvent::InterviewStarted { .. }
        | WorkflowRunEvent::InterviewTimeout { .. }
        | WorkflowRunEvent::Failover { .. } => {
            let mut fields = tagged_variant_fields(event);
            let node_id = remove_string(&mut fields, "stage");
            let node_label = default_node_label(node_id.as_ref(), None);
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id,
                node_label,
                properties: Value::Object(fields),
            }
        }
        WorkflowRunEvent::StallWatchdogTimeout { .. } => {
            let mut fields = tagged_variant_fields(event);
            let node_id = remove_string(&mut fields, "node");
            let node_label = default_node_label(node_id.as_ref(), None);
            EnvelopeFields {
                session_id: None,
                parent_session_id: None,
                node_id,
                node_label,
                properties: Value::Object(fields),
            }
        }
        _ => EnvelopeFields {
            session_id: None,
            parent_session_id: None,
            node_id: None,
            node_label: None,
            properties: Value::Object(tagged_variant_fields(event)),
        },
    }
}

pub fn canonicalize_event(run_id: &RunId, event: &WorkflowRunEvent) -> RunEventEnvelope {
    canonicalize_event_at(run_id, event, Utc::now())
}

pub fn canonicalize_event_at(
    run_id: &RunId,
    event: &WorkflowRunEvent,
    ts: chrono::DateTime<Utc>,
) -> RunEventEnvelope {
    let fields = extract_envelope_fields(event);
    RunEventEnvelope {
        id: Uuid::now_v7().to_string(),
        ts: ts.to_rfc3339_opts(SecondsFormat::Millis, true),
        run_id: run_id.to_string(),
        event: event_name(event).to_string(),
        session_id: fields.session_id,
        parent_session_id: fields.parent_session_id,
        node_id: fields.node_id,
        node_label: fields.node_label,
        properties: fields.properties,
    }
}

pub fn to_stored_event(run_id: &RunId, event: &WorkflowRunEvent) -> StoredEvent {
    to_stored_event_at(run_id, event, Utc::now())
}

pub fn to_stored_event_at(
    run_id: &RunId,
    event: &WorkflowRunEvent,
    ts: chrono::DateTime<Utc>,
) -> StoredEvent {
    let envelope = canonicalize_event_at(run_id, event, ts);
    let mut stored = StoredEvent::from(&envelope);

    match (event, &mut stored.body) {
        (WorkflowRunEvent::StageCompleted { failure, .. }, EventBody::StageCompleted(props)) => {
            props.failure = failure.clone();
        }
        (WorkflowRunEvent::StageFailed { failure, .. }, EventBody::StageFailed(props)) => {
            props.failure = Some(failure.clone());
        }
        _ => {}
    }

    stored.refresh_cache();
    stored
}

pub fn build_redacted_event_payload(event: &StoredEvent, run_id: &RunId) -> Result<EventPayload> {
    let line = redacted_event_json(event)?;
    event_payload_from_redacted_json(&line, run_id)
}

pub fn redacted_event_json(event: &StoredEvent) -> Result<String> {
    let line = serde_json::to_string(&normalized_event_value(event)?)?;
    Ok(redact_jsonl_line(&line))
}

fn normalized_event_value(event: &StoredEvent) -> Result<Value> {
    let value = event.to_value()?;
    Ok(normalize_json_value(value))
}

pub(crate) fn normalize_json_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, normalize_json_value(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(normalize_json_value).collect())
        }
        other => other,
    }
}

pub fn event_payload_from_redacted_json(line: &str, run_id: &RunId) -> Result<EventPayload> {
    let value = normalize_json_value(
        serde_json::from_str(line).context("Failed to parse redacted event payload")?,
    );
    EventPayload::new(value, run_id).map_err(anyhow::Error::from)
}

pub async fn append_workflow_event(
    run_store: &SlateRunStore,
    run_id: &RunId,
    event: &WorkflowRunEvent,
) -> Result<()> {
    let stored = to_stored_event(run_id, event);
    let payload = build_redacted_event_payload(&stored, run_id)?;
    run_store
        .append_event(&payload)
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

enum StoreProgressCommand {
    Event(EventPayload),
    Flush(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct StoreProgressLogger {
    tx: mpsc::UnboundedSender<StoreProgressCommand>,
}

impl StoreProgressLogger {
    #[must_use]
    pub fn new(run_store: SlateRunStore) -> Self {
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

        Self { tx }
    }

    pub fn register(&self, emitter: &EventEmitter) {
        let tx = self.tx.clone();
        emitter.on_event(
            move |event| match build_redacted_event_payload(event, &event.run_id) {
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
            },
        );
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

/// Current time as epoch milliseconds.
fn epoch_millis() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap()
}

/// Listener callback type for workflow run events.
type EventListener = Arc<dyn Fn(&StoredEvent) + Send + Sync>;

/// Callback-based event emitter for workflow run events.
pub struct EventEmitter {
    run_id: RunId,
    listeners: std::sync::Mutex<Vec<EventListener>>,
    /// Epoch milliseconds of the last `emit()` or `touch()` call. 0 until first event.
    last_event_at: AtomicI64,
}

impl std::fmt::Debug for EventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.listeners.lock().map(|l| l.len()).unwrap_or(0);
        f.debug_struct("EventEmitter")
            .field("run_id", &self.run_id)
            .field("listener_count", &count)
            .field("last_event_at", &self.last_event_at.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self::new(RunId::new())
    }
}

impl EventEmitter {
    #[must_use]
    pub fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            listeners: std::sync::Mutex::new(Vec::new()),
            last_event_at: AtomicI64::new(0),
        }
    }

    #[must_use]
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn on_event(&self, listener: impl Fn(&StoredEvent) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .expect("listeners lock poisoned")
            .push(Arc::new(listener));
    }

    pub fn emit(&self, event: &WorkflowRunEvent) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
        event.trace();
        if let WorkflowRunEvent::WorkflowRunStarted { run_id, .. } = event {
            debug_assert_eq!(
                *run_id, self.run_id,
                "workflow run started event must match emitter run_id"
            );
        }
        let stored = to_stored_event(&self.run_id, event);
        self.dispatch_stored_event(&stored);
    }

    pub(crate) fn dispatch_stored_event(&self, event: &StoredEvent) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
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
    use fabro_types::fixtures;
    use std::sync::{Arc, Mutex};

    #[test]
    fn event_emitter_new_has_no_listeners() {
        let emitter = EventEmitter::new(fixtures::RUN_1);
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }

    #[test]
    fn event_emitter_calls_listener_with_envelope() {
        let emitter = EventEmitter::new(fixtures::RUN_1);
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        emitter.on_event(move |event| {
            received_clone.lock().unwrap().push(event.clone());
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
        assert_eq!(events[0].event_name(), "run.started");
        assert_eq!(events[0].run_id, fixtures::RUN_1);
        assert!(events[0].id.len() >= 32);
    }

    #[test]
    fn event_emitter_default() {
        let emitter = EventEmitter::default();
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }

    #[test]
    fn canonicalize_stage_completed_places_node_fields_in_envelope() {
        let envelope = canonicalize_event(
            &fixtures::RUN_2,
            &WorkflowRunEvent::StageCompleted {
                node_id: "plan".to_string(),
                name: "Plan".to_string(),
                index: 0,
                duration_ms: 5000,
                status: "success".to_string(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                usage: None,
                failure: None,
                notes: None,
                files_touched: Vec::new(),
                context_updates: None,
                jump_to_node: None,
                context_values: None,
                node_visits: None,
                loop_failure_signatures: None,
                restart_failure_signatures: None,
                response: None,
                attempt: 1,
                max_attempts: 1,
            },
        );

        assert_eq!(envelope.event, "stage.completed");
        assert_eq!(envelope.run_id, fixtures::RUN_2.to_string());
        assert_eq!(envelope.node_id.as_deref(), Some("plan"));
        assert_eq!(envelope.node_label.as_deref(), Some("Plan"));
        assert_eq!(envelope.properties["duration_ms"], 5000);
        assert_eq!(envelope.properties["status"], "success");
        assert!(envelope.session_id.is_none());
    }

    #[test]
    fn canonicalize_stage_completed_keeps_response_and_signature_snapshots() {
        let envelope = canonicalize_event(
            &fixtures::RUN_2,
            &WorkflowRunEvent::StageCompleted {
                node_id: "plan".to_string(),
                name: "Plan".to_string(),
                index: 0,
                duration_ms: 5000,
                status: "success".to_string(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                usage: None,
                failure: None,
                notes: None,
                files_touched: Vec::new(),
                context_updates: None,
                jump_to_node: None,
                context_values: None,
                node_visits: None,
                loop_failure_signatures: Some(BTreeMap::from([("sig-a".to_string(), 2usize)])),
                restart_failure_signatures: Some(BTreeMap::from([("sig-b".to_string(), 1usize)])),
                response: Some("done".to_string()),
                attempt: 1,
                max_attempts: 1,
            },
        );

        assert_eq!(envelope.properties["response"], "done");
        assert_eq!(envelope.properties["loop_failure_signatures"]["sig-a"], 2);
        assert_eq!(
            envelope.properties["restart_failure_signatures"]["sig-b"],
            1
        );
    }

    #[test]
    fn canonicalize_stage_failure_flattens_failure_detail() {
        let envelope = canonicalize_event(
            &fixtures::RUN_3,
            &WorkflowRunEvent::StageFailed {
                node_id: "code".to_string(),
                name: "Code".to_string(),
                index: 1,
                failure: FailureDetail::new(
                    "lint failed",
                    crate::outcome::FailureCategory::Deterministic,
                ),
                will_retry: true,
            },
        );

        assert_eq!(envelope.event, "stage.failed");
        assert_eq!(envelope.properties["error"], "lint failed");
        assert_eq!(envelope.properties["failure_class"], "deterministic");
        assert_eq!(envelope.properties["will_retry"], true);
        assert!(envelope.properties.get("failure").is_none());
    }

    #[test]
    fn canonicalize_agent_tool_started_moves_session_metadata_to_envelope() {
        let envelope = canonicalize_event(
            &fixtures::RUN_4,
            &WorkflowRunEvent::Agent {
                stage: "code".to_string(),
                visit: 2,
                event: AgentEvent::ToolCallStarted {
                    tool_name: "read_file".to_string(),
                    tool_call_id: "call_1".to_string(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                },
                session_id: Some("ses_child".to_string()),
                parent_session_id: Some("ses_parent".to_string()),
            },
        );

        assert_eq!(envelope.event, "agent.tool.started");
        assert_eq!(envelope.node_id.as_deref(), Some("code"));
        assert_eq!(envelope.node_label.as_deref(), Some("code"));
        assert_eq!(envelope.session_id.as_deref(), Some("ses_child"));
        assert_eq!(envelope.parent_session_id.as_deref(), Some("ses_parent"));
        assert_eq!(envelope.properties["tool_name"], "read_file");
        assert_eq!(envelope.properties["tool_call_id"], "call_1");
        assert_eq!(envelope.properties["visit"], 2);
    }

    #[test]
    fn canonicalize_sandbox_event_keeps_properties_nested() {
        let envelope = canonicalize_event(
            &fixtures::RUN_5,
            &WorkflowRunEvent::Sandbox {
                event: SandboxEvent::Ready {
                    provider: "daytona".to_string(),
                    duration_ms: 2500,
                    name: Some("sandbox-1".to_string()),
                    cpu: Some(4.0),
                    memory: Some(8.0),
                    url: Some("https://example.test".to_string()),
                },
            },
        );

        assert_eq!(envelope.event, "sandbox.ready");
        assert!(envelope.node_id.is_none());
        assert_eq!(envelope.properties["provider"], "daytona");
        assert_eq!(envelope.properties["duration_ms"], 2500);
    }

    #[test]
    fn canonicalize_workflow_failure_flattens_error_display() {
        let envelope = canonicalize_event(
            &fixtures::RUN_6,
            &WorkflowRunEvent::WorkflowRunFailed {
                error: FabroError::handler("boom"),
                duration_ms: 900,
                reason: Some(StatusReason::WorkflowError),
                git_commit_sha: Some("abc123".to_string()),
            },
        );

        assert_eq!(envelope.event, "run.failed");
        assert_eq!(envelope.properties["error"], "Handler error: boom");
        assert_eq!(envelope.properties["duration_ms"], 900);
    }

    #[tokio::test]
    async fn append_workflow_event_writes_store_event_shape() {
        let store = fabro_store::SlateStore::new(
            std::sync::Arc::new(object_store::memory::InMemory::new()),
            "",
            std::time::Duration::from_millis(1),
        );
        let run_store = store.create_run(&fixtures::RUN_7).await.unwrap();
        let envelope = canonicalize_event(
            &fixtures::RUN_7,
            &WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "example".to_string(),
                message: "notice".to_string(),
            },
        );

        let stored = StoredEvent::from(&envelope);
        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_7).unwrap();
        run_store.append_event(&payload).await.unwrap();

        let events = run_store.list_events().await.unwrap();
        let line = events
            .into_iter()
            .next()
            .map(|event| event.payload.as_value().clone())
            .unwrap();
        assert!(line.get("id").is_some());
        assert_eq!(line["event"], "run.notice");
        assert_eq!(line["properties"]["code"], "example");
    }

    #[test]
    fn build_redacted_event_payload_requires_id() {
        let envelope = canonicalize_event(
            &fixtures::RUN_8,
            &WorkflowRunEvent::RetroStarted {
                prompt: Some("Analyze the run".to_string()),
                provider: None,
                model: None,
            },
        );

        let stored = StoredEvent::from(&envelope);
        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_8).unwrap();
        assert_eq!(payload.as_value()["id"], envelope.id);
        assert_eq!(payload.as_value()["event"], "retro.started");
        assert_eq!(
            payload.as_value()["properties"]["prompt"],
            "Analyze the run"
        );
    }

    #[test]
    fn event_name_matches_new_dot_notation() {
        assert_eq!(
            event_name(&WorkflowRunEvent::RetroStarted {
                prompt: None,
                provider: None,
                model: None,
            }),
            "retro.started"
        );
        assert_eq!(
            event_name(&WorkflowRunEvent::ParallelBranchStarted {
                branch: "fork".to_string(),
                index: 0,
            }),
            "parallel.branch.started"
        );
        assert_eq!(
            event_name(&WorkflowRunEvent::Agent {
                stage: "code".to_string(),
                visit: 1,
                event: AgentEvent::SubAgentSpawned {
                    agent_id: "a1".to_string(),
                    depth: 1,
                    task: "do it".to_string(),
                },
                session_id: None,
                parent_session_id: None,
            }),
            "agent.sub.spawned"
        );
    }
}
