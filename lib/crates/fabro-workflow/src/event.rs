use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ::fabro_types::{
    ActorRef, BilledTokenCounts, ParallelBranchId, RunBlobId, RunControlAction, RunEvent, RunId,
    RunProvenance, StageId, StageStatus, StatusReason, run_event as fabro_types,
};
use anyhow::{Context, Result};
use chrono::Utc;
use fabro_agent::{AgentEvent, SandboxEvent, WorktreeEvent, WorktreeEventCallback};
use fabro_llm::types::TokenCounts as LlmTokenCounts;
use fabro_store::{EventPayload, RunDatabase};
pub use fabro_types::{EventBody, RunNoticeLevel};
use fabro_util::json::normalize_json_value;
use fabro_util::redact::redact_json_value;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use uuid::Uuid;

use crate::context::{Context as WfContext, WorkflowContext};
use crate::error::Error;
use crate::outcome::{BilledModelUsage, FailureDetail, Outcome};
use crate::run_dir::visit_from_context;
use crate::runtime_store::RunStoreHandle;

/// Events emitted during workflow run execution for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    RunCreated {
        run_id:            RunId,
        settings:          serde_json::Value,
        graph:             serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_source:   Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_config:   Option<String>,
        labels:            BTreeMap<String, String>,
        run_dir:           String,
        working_directory: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_repo_path:    Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo_origin_url:   Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_branch:       Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_slug:     Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        db_prefix:         Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance:        Option<RunProvenance>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manifest_blob:     Option<RunBlobId>,
    },
    WorkflowRunStarted {
        name:         String,
        run_id:       RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_branch:  Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_sha:     Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_branch:   Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal:         Option<String>,
    },
    RunSubmitted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason:          Option<StatusReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        definition_blob: Option<RunBlobId>,
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
    RunCancelRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<ActorRef>,
    },
    RunPauseRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<ActorRef>,
    },
    RunUnpauseRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<ActorRef>,
    },
    RunPaused,
    RunUnpaused,
    RunRewound {
        target_checkpoint_ordinal: usize,
        target_node_id:            String,
        target_visit:              usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_status:           Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_commit_sha:            Option<String>,
    },
    WorkflowRunCompleted {
        duration_ms:          u64,
        artifact_count:       usize,
        #[serde(default)]
        status:               String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason:               Option<StatusReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_usd_micros:     Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_git_commit_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_patch:          Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing:              Option<BilledTokenCounts>,
    },
    WorkflowRunFailed {
        error:          Error,
        duration_ms:    u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason:         Option<StatusReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_commit_sha: Option<String>,
    },
    RunNotice {
        level:   RunNoticeLevel,
        code:    String,
        message: String,
    },
    StageStarted {
        node_id:      String,
        name:         String,
        index:        usize,
        handler_type: String,
        attempt:      usize,
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
        billing: Option<BilledModelUsage>,
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
        node_id:    String,
        name:       String,
        index:      usize,
        failure:    FailureDetail,
        will_retry: bool,
    },
    StageRetrying {
        node_id:      String,
        name:         String,
        index:        usize,
        attempt:      usize,
        max_attempts: usize,
        delay_ms:     u64,
    },
    ParallelStarted {
        node_id:      String,
        visit:        u32,
        branch_count: usize,
        join_policy:  String,
    },
    ParallelBranchStarted {
        parallel_group_id:  StageId,
        parallel_branch_id: ParallelBranchId,
        branch:             String,
        index:              usize,
    },
    ParallelBranchCompleted {
        parallel_group_id:  StageId,
        parallel_branch_id: ParallelBranchId,
        branch:             String,
        index:              usize,
        duration_ms:        u64,
        status:             String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_sha:           Option<String>,
    },
    ParallelCompleted {
        node_id:       String,
        visit:         u32,
        duration_ms:   u64,
        success_count: usize,
        failure_count: usize,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        results:       Vec<serde_json::Value>,
    },
    InterviewStarted {
        question_id:     String,
        question:        String,
        stage:           String,
        question_type:   String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options:         Vec<fabro_types::InterviewOption>,
        #[serde(default)]
        allow_freeform:  bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_display: Option<String>,
    },
    InterviewCompleted {
        question_id: String,
        question:    String,
        answer:      String,
        duration_ms: u64,
    },
    InterviewTimeout {
        question_id: String,
        question:    String,
        stage:       String,
        duration_ms: u64,
    },
    InterviewInterrupted {
        question_id: String,
        question:    String,
        stage:       String,
        reason:      String,
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
        error:   String,
    },
    GitCommit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        sha:     String,
    },
    GitPush {
        branch:  String,
        success: bool,
    },
    GitBranch {
        branch: String,
        sha:    String,
    },
    GitWorktreeAdd {
        path:   String,
        branch: String,
    },
    GitWorktreeRemove {
        path: String,
    },
    GitFetch {
        branch:  String,
        success: bool,
    },
    GitReset {
        sha: String,
    },
    EdgeSelected {
        from_node:          String,
        to_node:            String,
        label:              Option<String>,
        condition:          Option<String>,
        /// Which selection step chose this edge (e.g. "condition",
        /// "preferred_label", "jump").
        reason:             String,
        /// The stage's preferred label hint, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preferred_label:    Option<String>,
        /// The stage's suggested next node IDs, if any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggested_next_ids: Vec<String>,
        /// The stage outcome status that influenced routing.
        stage_status:       String,
        /// Whether this was a direct jump (bypassing normal edge selection).
        is_jump:            bool,
    },
    LoopRestart {
        from_node: String,
        to_node:   String,
    },
    Prompt {
        stage:    String,
        visit:    u32,
        text:     String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode:     Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model:    Option<String>,
    },
    PromptCompleted {
        node_id:  String,
        response: String,
        model:    String,
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing:  Option<BilledModelUsage>,
    },
    /// Forwarded from an agent session, tagged with the workflow stage.
    Agent {
        stage:             String,
        visit:             u32,
        event:             AgentEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id:        Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
    },
    SubgraphStarted {
        node_id:    String,
        start_node: String,
    },
    SubgraphCompleted {
        node_id:        String,
        steps_executed: usize,
        status:         String,
        duration_ms:    u64,
    },
    /// Forwarded from a sandbox lifecycle operation.
    Sandbox {
        event: SandboxEvent,
    },
    /// Emitted after the sandbox has been initialized (by engine lifecycle).
    SandboxInitialized {
        working_directory:      String,
        provider:               String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identifier:             Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_working_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        container_mount_point:  Option<String>,
    },
    SetupStarted {
        command_count: usize,
    },
    SetupCommandStarted {
        command: String,
        index:   usize,
    },
    SetupCommandCompleted {
        command:     String,
        index:       usize,
        exit_code:   i32,
        duration_ms: u64,
    },
    SetupCompleted {
        duration_ms: u64,
    },
    SetupFailed {
        command:   String,
        index:     usize,
        exit_code: i32,
        stderr:    String,
    },
    StallWatchdogTimeout {
        node:         String,
        idle_seconds: u64,
    },
    ArtifactCaptured {
        node_id:        String,
        attempt:        u32,
        node_slug:      String,
        path:           String,
        mime:           String,
        content_md5:    String,
        content_sha256: String,
        bytes:          u64,
    },
    SshAccessReady {
        ssh_command: String,
    },
    Failover {
        stage:         String,
        from_provider: String,
        from_model:    String,
        to_provider:   String,
        to_model:      String,
        error:         String,
    },
    CliEnsureStarted {
        cli_name: String,
        provider: String,
    },
    CliEnsureCompleted {
        cli_name:          String,
        provider:          String,
        already_installed: bool,
        node_installed:    bool,
        duration_ms:       u64,
    },
    CliEnsureFailed {
        cli_name:    String,
        provider:    String,
        error:       String,
        duration_ms: u64,
    },
    CommandStarted {
        node_id:    String,
        script:     String,
        command:    String,
        language:   String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    CommandCompleted {
        node_id:     String,
        stdout:      String,
        stderr:      String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code:   Option<i32>,
        duration_ms: u64,
        timed_out:   bool,
    },
    AgentCliStarted {
        node_id:  String,
        visit:    u32,
        mode:     String,
        provider: String,
        model:    String,
        command:  String,
    },
    AgentCliCompleted {
        node_id:     String,
        stdout:      String,
        stderr:      String,
        exit_code:   i32,
        duration_ms: u64,
    },
    PullRequestCreated {
        pr_url:      String,
        pr_number:   u64,
        owner:       String,
        repo:        String,
        base_branch: String,
        head_branch: String,
        title:       String,
        draft:       bool,
    },
    PullRequestFailed {
        error: String,
    },
    DevcontainerResolved {
        dockerfile_lines:        usize,
        environment_count:       usize,
        lifecycle_command_count: usize,
        workspace_folder:        String,
    },
    DevcontainerLifecycleStarted {
        phase:         String,
        command_count: usize,
    },
    DevcontainerLifecycleCommandStarted {
        phase:   String,
        command: String,
        index:   usize,
    },
    DevcontainerLifecycleCommandCompleted {
        phase:       String,
        command:     String,
        index:       usize,
        exit_code:   i32,
        duration_ms: u64,
    },
    DevcontainerLifecycleCompleted {
        phase:       String,
        duration_ms: u64,
    },
    DevcontainerLifecycleFailed {
        phase:     String,
        command:   String,
        index:     usize,
        exit_code: i32,
        stderr:    String,
    },
    RetroStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt:   Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model:    Option<String>,
    },
    RetroCompleted {
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response:    Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retro:       Option<serde_json::Value>,
    },
    RetroFailed {
        error:       String,
        duration_ms: u64,
    },
}

impl Event {
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
            Self::RunSubmitted {
                reason,
                definition_blob,
            } => {
                info!(?reason, ?definition_blob, "Run submitted");
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
            Self::RunCancelRequested { .. } => {
                info!("Run cancel requested");
            }
            Self::RunPauseRequested { .. } => {
                info!("Run pause requested");
            }
            Self::RunUnpauseRequested { .. } => {
                info!("Run unpause requested");
            }
            Self::RunPaused => {
                info!("Run paused");
            }
            Self::RunUnpaused => {
                info!("Run unpaused");
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
                ..
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
                ..
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
            Self::ParallelBranchStarted { branch, index, .. } => {
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
            Self::InterviewInterrupted {
                stage,
                reason,
                duration_ms,
                ..
            } => {
                warn!(stage, reason, duration_ms, "Interview interrupted");
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
            Self::ArtifactCaptured {
                node_id,
                node_slug,
                attempt,
                path,
                bytes,
                ..
            } => {
                debug!(
                    node_id,
                    node_slug, attempt, path, bytes, "Artifact captured"
                );
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

#[must_use]
pub fn event_name(event: &Event) -> &'static str {
    match event {
        Event::RunCreated { .. } => "run.created",
        Event::WorkflowRunStarted { .. } => "run.started",
        Event::RunSubmitted { .. } => "run.submitted",
        Event::RunStarting { .. } => "run.starting",
        Event::RunRunning { .. } => "run.running",
        Event::RunRemoving { .. } => "run.removing",
        Event::RunCancelRequested { .. } => "run.cancel.requested",
        Event::RunPauseRequested { .. } => "run.pause.requested",
        Event::RunUnpauseRequested { .. } => "run.unpause.requested",
        Event::RunPaused => "run.paused",
        Event::RunUnpaused => "run.unpaused",
        Event::RunRewound { .. } => "run.rewound",
        Event::WorkflowRunCompleted { .. } => "run.completed",
        Event::WorkflowRunFailed { .. } => "run.failed",
        Event::RunNotice { .. } => "run.notice",
        Event::StageStarted { .. } => "stage.started",
        Event::StageCompleted { .. } => "stage.completed",
        Event::StageFailed { .. } => "stage.failed",
        Event::StageRetrying { .. } => "stage.retrying",
        Event::ParallelStarted { .. } => "parallel.started",
        Event::ParallelBranchStarted { .. } => "parallel.branch.started",
        Event::ParallelBranchCompleted { .. } => "parallel.branch.completed",
        Event::ParallelCompleted { .. } => "parallel.completed",
        Event::InterviewStarted { .. } => "interview.started",
        Event::InterviewCompleted { .. } => "interview.completed",
        Event::InterviewTimeout { .. } => "interview.timeout",
        Event::InterviewInterrupted { .. } => "interview.interrupted",
        Event::CheckpointCompleted { .. } => "checkpoint.completed",
        Event::CheckpointFailed { .. } => "checkpoint.failed",
        Event::GitCommit { .. } => "git.commit",
        Event::GitPush { .. } => "git.push",
        Event::GitBranch { .. } => "git.branch",
        Event::GitWorktreeAdd { .. } => "git.worktree.added",
        Event::GitWorktreeRemove { .. } => "git.worktree.removed",
        Event::GitFetch { .. } => "git.fetch",
        Event::GitReset { .. } => "git.reset",
        Event::EdgeSelected { .. } => "edge.selected",
        Event::LoopRestart { .. } => "loop.restart",
        Event::Prompt { .. } => "stage.prompt",
        Event::PromptCompleted { .. } => "prompt.completed",
        Event::Agent { event, .. } => match event {
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
        Event::SubgraphStarted { .. } => "subgraph.started",
        Event::SubgraphCompleted { .. } => "subgraph.completed",
        Event::Sandbox { event } => match event {
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
        Event::SandboxInitialized { .. } => "sandbox.initialized",
        Event::SetupStarted { .. } => "setup.started",
        Event::SetupCommandStarted { .. } => "setup.command.started",
        Event::SetupCommandCompleted { .. } => "setup.command.completed",
        Event::SetupCompleted { .. } => "setup.completed",
        Event::SetupFailed { .. } => "setup.failed",
        Event::StallWatchdogTimeout { .. } => "watchdog.timeout",
        Event::ArtifactCaptured { .. } => "artifact.captured",
        Event::SshAccessReady { .. } => "ssh.ready",
        Event::Failover { .. } => "agent.failover",
        Event::CliEnsureStarted { .. } => "cli.ensure.started",
        Event::CliEnsureCompleted { .. } => "cli.ensure.completed",
        Event::CliEnsureFailed { .. } => "cli.ensure.failed",
        Event::CommandStarted { .. } => "command.started",
        Event::CommandCompleted { .. } => "command.completed",
        Event::AgentCliStarted { .. } => "agent.cli.started",
        Event::AgentCliCompleted { .. } => "agent.cli.completed",
        Event::PullRequestCreated { .. } => "pull_request.created",
        Event::PullRequestFailed { .. } => "pull_request.failed",
        Event::DevcontainerResolved { .. } => "devcontainer.resolved",
        Event::DevcontainerLifecycleStarted { .. } => "devcontainer.lifecycle.started",
        Event::DevcontainerLifecycleCommandStarted { .. } => {
            "devcontainer.lifecycle.command.started"
        }
        Event::DevcontainerLifecycleCommandCompleted { .. } => {
            "devcontainer.lifecycle.command.completed"
        }
        Event::DevcontainerLifecycleCompleted { .. } => "devcontainer.lifecycle.completed",
        Event::DevcontainerLifecycleFailed { .. } => "devcontainer.lifecycle.failed",
        Event::RetroStarted { .. } => "retro.started",
        Event::RetroCompleted { .. } => "retro.completed",
        Event::RetroFailed { .. } => "retro.failed",
    }
}

#[derive(Debug, Default)]
struct StoredEventFields {
    session_id:         Option<String>,
    parent_session_id:  Option<String>,
    node_id:            Option<String>,
    node_label:         Option<String>,
    stage_id:           Option<StageId>,
    parallel_group_id:  Option<StageId>,
    parallel_branch_id: Option<ParallelBranchId>,
    tool_call_id:       Option<String>,
    actor:              Option<ActorRef>,
}

fn default_node_label(node_id: Option<&String>, node_label: Option<String>) -> Option<String> {
    node_label.or_else(|| node_id.cloned())
}

fn node_stored_fields(node_id: Option<String>) -> StoredEventFields {
    let node_label = default_node_label(node_id.as_ref(), None);
    StoredEventFields {
        node_id,
        node_label,
        ..StoredEventFields::default()
    }
}

fn billed_token_counts_from_llm(usage: &LlmTokenCounts) -> BilledTokenCounts {
    BilledTokenCounts {
        input_tokens:       usage.input_tokens,
        output_tokens:      usage.output_tokens,
        total_tokens:       usage.total_tokens(),
        reasoning_tokens:   usage.reasoning_tokens,
        cache_read_tokens:  usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        total_usd_micros:   None,
    }
}

fn stage_status_from_string(status: &str) -> StageStatus {
    serde_json::from_value(Value::String(status.to_string())).expect("valid stage status")
}

fn stored_event_fields(event: &Event, scope: Option<&StageScope>) -> StoredEventFields {
    let mut fields = stored_event_fields_for_variant(event);
    if let Some(scope) = scope {
        if fields.node_id.is_none() {
            fields.node_id = Some(scope.node_id.clone());
            fields.node_label = default_node_label(Some(&scope.node_id), fields.node_label);
        }
        if fields.stage_id.is_none() {
            fields.stage_id = Some(StageId::new(scope.node_id.clone(), scope.visit));
        }
        if fields.parallel_group_id.is_none() {
            fields
                .parallel_group_id
                .clone_from(&scope.parallel_group_id);
        }
        if fields.parallel_branch_id.is_none() {
            fields
                .parallel_branch_id
                .clone_from(&scope.parallel_branch_id);
        }
    }
    fields
}

fn stored_event_fields_for_variant(event: &Event) -> StoredEventFields {
    match event {
        Event::RunCreated { provenance, .. } => StoredEventFields {
            actor: provenance.as_ref().and_then(actor_from_provenance),
            ..StoredEventFields::default()
        },
        Event::RunCancelRequested { actor }
        | Event::RunPauseRequested { actor }
        | Event::RunUnpauseRequested { actor } => StoredEventFields {
            actor: actor.clone(),
            ..StoredEventFields::default()
        },
        Event::StageCompleted { node_id, name, .. }
        | Event::StageFailed { node_id, name, .. }
        | Event::StageStarted { node_id, name, .. }
        | Event::StageRetrying { node_id, name, .. } => {
            let node_id_str = node_id.clone();
            let node_label = default_node_label(Some(&node_id_str), Some(name.clone()));
            StoredEventFields {
                node_id: Some(node_id_str),
                node_label,
                ..StoredEventFields::default()
            }
        }
        Event::ParallelStarted { node_id, visit, .. }
        | Event::ParallelCompleted { node_id, visit, .. } => {
            let node_id_str = node_id.clone();
            let node_label = default_node_label(Some(&node_id_str), None);
            let parallel_group_id = Some(StageId::new(node_id_str.clone(), *visit));
            StoredEventFields {
                node_id: Some(node_id_str),
                node_label,
                parallel_group_id,
                ..StoredEventFields::default()
            }
        }
        Event::CheckpointCompleted { node_id, .. }
        | Event::CheckpointFailed { node_id, .. }
        | Event::SubgraphStarted { node_id, .. }
        | Event::SubgraphCompleted { node_id, .. }
        | Event::ArtifactCaptured { node_id, .. }
        | Event::PromptCompleted { node_id, .. }
        | Event::CommandStarted { node_id, .. }
        | Event::CommandCompleted { node_id, .. }
        | Event::AgentCliStarted { node_id, .. }
        | Event::AgentCliCompleted { node_id, .. } => node_stored_fields(Some(node_id.clone())),
        Event::Agent {
            stage,
            visit,
            event: agent_event,
            session_id,
            parent_session_id,
        } => {
            let node_id = Some(stage.clone());
            let node_label = default_node_label(node_id.as_ref(), None);
            let stage_id = Some(StageId::new(stage.clone(), *visit));
            let tool_call_id = agent_tool_call_id(agent_event).map(str::to_string);
            let actor = agent_actor_for_event(agent_event, session_id.as_deref());
            StoredEventFields {
                session_id: session_id.clone(),
                parent_session_id: parent_session_id.clone(),
                node_id,
                node_label,
                stage_id,
                tool_call_id,
                actor,
                ..StoredEventFields::default()
            }
        }
        Event::GitCommit { node_id, .. } => node_stored_fields(node_id.clone()),
        Event::ParallelBranchStarted {
            parallel_group_id,
            parallel_branch_id,
            branch,
            ..
        }
        | Event::ParallelBranchCompleted {
            parallel_group_id,
            parallel_branch_id,
            branch,
            ..
        } => {
            let node_id = Some(branch.clone());
            let node_label = default_node_label(node_id.as_ref(), None);
            StoredEventFields {
                node_id,
                node_label,
                parallel_group_id: Some(parallel_group_id.clone()),
                parallel_branch_id: Some(parallel_branch_id.clone()),
                ..StoredEventFields::default()
            }
        }
        Event::Prompt { stage, .. }
        | Event::InterviewStarted { stage, .. }
        | Event::InterviewTimeout { stage, .. }
        | Event::InterviewInterrupted { stage, .. }
        | Event::Failover { stage, .. } => node_stored_fields(Some(stage.clone())),
        Event::StallWatchdogTimeout { node, .. } => node_stored_fields(Some(node.clone())),
        _ => StoredEventFields::default(),
    }
}

fn actor_from_provenance(provenance: &RunProvenance) -> Option<ActorRef> {
    provenance
        .subject
        .as_ref()?
        .login
        .clone()
        .map(ActorRef::user)
}

fn agent_tool_call_id(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::ToolCallStarted { tool_call_id, .. }
        | AgentEvent::ToolCallCompleted { tool_call_id, .. } => Some(tool_call_id.as_str()),
        _ => None,
    }
}

fn agent_actor_for_event(event: &AgentEvent, session_id: Option<&str>) -> Option<ActorRef> {
    match event {
        AgentEvent::AssistantMessage { model, .. } => Some(ActorRef::agent(
            session_id.map(str::to_string),
            Some(model.clone()),
        )),
        _ => None,
    }
}

fn event_body_from_event(event: &Event) -> EventBody {
    match event {
        Event::RunCreated {
            settings,
            graph,
            workflow_source,
            workflow_config,
            labels,
            run_dir,
            working_directory,
            host_repo_path,
            repo_origin_url,
            base_branch,
            workflow_slug,
            db_prefix,
            provenance,
            manifest_blob,
            ..
        } => EventBody::RunCreated(fabro_types::RunCreatedProps {
            settings:          serde_json::from_value(settings.clone())
                .expect("run.created settings"),
            graph:             serde_json::from_value(graph.clone()).expect("run.created graph"),
            workflow_source:   workflow_source.clone(),
            workflow_config:   workflow_config.clone(),
            labels:            labels.clone(),
            run_dir:           run_dir.clone(),
            working_directory: working_directory.clone(),
            host_repo_path:    host_repo_path.clone(),
            repo_origin_url:   repo_origin_url.clone(),
            base_branch:       base_branch.clone(),
            workflow_slug:     workflow_slug.clone(),
            db_prefix:         db_prefix.clone(),
            provenance:        provenance.clone(),
            manifest_blob:     *manifest_blob,
        }),
        Event::WorkflowRunStarted {
            name,
            base_branch,
            base_sha,
            run_branch,
            worktree_dir,
            goal,
            ..
        } => EventBody::RunStarted(fabro_types::RunStartedProps {
            name:         name.clone(),
            base_branch:  base_branch.clone(),
            base_sha:     base_sha.clone(),
            run_branch:   run_branch.clone(),
            worktree_dir: worktree_dir.clone(),
            goal:         goal.clone(),
        }),
        Event::RunSubmitted {
            reason,
            definition_blob,
        } => EventBody::RunSubmitted(fabro_types::RunSubmittedProps {
            reason:          *reason,
            definition_blob: *definition_blob,
        }),
        Event::RunStarting { reason } => {
            EventBody::RunStarting(fabro_types::RunStatusTransitionProps { reason: *reason })
        }
        Event::RunRunning { reason } => {
            EventBody::RunRunning(fabro_types::RunStatusTransitionProps { reason: *reason })
        }
        Event::RunRemoving { reason } => {
            EventBody::RunRemoving(fabro_types::RunStatusTransitionProps { reason: *reason })
        }
        Event::RunCancelRequested { .. } => {
            EventBody::RunCancelRequested(fabro_types::RunControlRequestedProps {
                action: RunControlAction::Cancel,
            })
        }
        Event::RunPauseRequested { .. } => {
            EventBody::RunPauseRequested(fabro_types::RunControlRequestedProps {
                action: RunControlAction::Pause,
            })
        }
        Event::RunUnpauseRequested { .. } => {
            EventBody::RunUnpauseRequested(fabro_types::RunControlRequestedProps {
                action: RunControlAction::Unpause,
            })
        }
        Event::RunPaused => EventBody::RunPaused(fabro_types::RunControlEffectProps::default()),
        Event::RunUnpaused => EventBody::RunUnpaused(fabro_types::RunControlEffectProps::default()),
        Event::RunRewound {
            target_checkpoint_ordinal,
            target_node_id,
            target_visit,
            previous_status,
            run_commit_sha,
        } => EventBody::RunRewound(fabro_types::RunRewoundProps {
            target_checkpoint_ordinal: *target_checkpoint_ordinal,
            target_node_id:            target_node_id.clone(),
            target_visit:              *target_visit,
            previous_status:           previous_status.clone(),
            run_commit_sha:            run_commit_sha.clone(),
        }),
        Event::WorkflowRunCompleted {
            duration_ms,
            artifact_count,
            status,
            reason,
            total_usd_micros,
            final_git_commit_sha,
            final_patch,
            billing,
        } => EventBody::RunCompleted(fabro_types::RunCompletedProps {
            duration_ms:          *duration_ms,
            artifact_count:       *artifact_count,
            status:               status.clone(),
            reason:               *reason,
            total_usd_micros:     *total_usd_micros,
            final_git_commit_sha: final_git_commit_sha.clone(),
            final_patch:          final_patch.clone(),
            billing:              billing.clone(),
        }),
        Event::WorkflowRunFailed {
            error,
            duration_ms,
            reason,
            git_commit_sha,
        } => EventBody::RunFailed(fabro_types::RunFailedProps {
            error:          error.to_string(),
            duration_ms:    *duration_ms,
            reason:         *reason,
            git_commit_sha: git_commit_sha.clone(),
        }),
        Event::RunNotice {
            level,
            code,
            message,
        } => EventBody::RunNotice(fabro_types::RunNoticeProps {
            level:   *level,
            code:    code.clone(),
            message: message.clone(),
        }),
        Event::StageStarted {
            index,
            handler_type,
            attempt,
            max_attempts,
            ..
        } => EventBody::StageStarted(fabro_types::StageStartedProps {
            index:        *index,
            handler_type: handler_type.clone(),
            attempt:      *attempt,
            max_attempts: *max_attempts,
        }),
        Event::StageCompleted {
            index,
            duration_ms,
            status,
            preferred_label,
            suggested_next_ids,
            billing,
            failure,
            notes,
            files_touched,
            context_updates,
            jump_to_node,
            context_values,
            node_visits,
            loop_failure_signatures,
            restart_failure_signatures,
            response,
            attempt,
            max_attempts,
            ..
        } => EventBody::StageCompleted(fabro_types::StageCompletedProps {
            index: *index,
            duration_ms: *duration_ms,
            status: stage_status_from_string(status),
            preferred_label: preferred_label.clone(),
            suggested_next_ids: suggested_next_ids.clone(),
            billing: billing.clone(),
            failure: failure.clone(),
            notes: notes.clone(),
            files_touched: files_touched.clone(),
            context_updates: context_updates.clone(),
            jump_to_node: jump_to_node.clone(),
            context_values: context_values.clone(),
            node_visits: node_visits.clone(),
            loop_failure_signatures: loop_failure_signatures.clone(),
            restart_failure_signatures: restart_failure_signatures.clone(),
            response: response.clone(),
            attempt: *attempt,
            max_attempts: *max_attempts,
        }),
        Event::StageFailed {
            index,
            failure,
            will_retry,
            ..
        } => EventBody::StageFailed(fabro_types::StageFailedProps {
            index:      *index,
            failure:    Some(failure.clone()),
            will_retry: *will_retry,
        }),
        Event::StageRetrying {
            index,
            attempt,
            max_attempts,
            delay_ms,
            ..
        } => EventBody::StageRetrying(fabro_types::StageRetryingProps {
            index:        *index,
            attempt:      *attempt,
            max_attempts: *max_attempts,
            delay_ms:     *delay_ms,
        }),
        Event::ParallelStarted {
            visit,
            branch_count,
            join_policy,
            ..
        } => EventBody::ParallelStarted(fabro_types::ParallelStartedProps {
            visit:        *visit,
            branch_count: *branch_count,
            join_policy:  join_policy.clone(),
        }),
        Event::ParallelBranchStarted { index, .. } => {
            EventBody::ParallelBranchStarted(fabro_types::ParallelBranchStartedProps {
                index: *index,
            })
        }
        Event::ParallelBranchCompleted {
            index,
            duration_ms,
            status,
            head_sha,
            ..
        } => EventBody::ParallelBranchCompleted(fabro_types::ParallelBranchCompletedProps {
            index:       *index,
            duration_ms: *duration_ms,
            status:      status.clone(),
            head_sha:    head_sha.clone(),
        }),
        Event::ParallelCompleted {
            visit,
            duration_ms,
            success_count,
            failure_count,
            results,
            ..
        } => EventBody::ParallelCompleted(fabro_types::ParallelCompletedProps {
            visit:         *visit,
            duration_ms:   *duration_ms,
            success_count: *success_count,
            failure_count: *failure_count,
            results:       results.clone(),
        }),
        Event::InterviewStarted {
            question_id,
            question,
            stage,
            question_type,
            options,
            allow_freeform,
            timeout_seconds,
            context_display,
        } => EventBody::InterviewStarted(fabro_types::InterviewStartedProps {
            question_id:     question_id.clone(),
            question:        question.clone(),
            stage:           stage.clone(),
            question_type:   question_type.clone(),
            options:         options.clone(),
            allow_freeform:  *allow_freeform,
            timeout_seconds: *timeout_seconds,
            context_display: context_display.clone(),
        }),
        Event::InterviewCompleted {
            question_id,
            question,
            answer,
            duration_ms,
        } => EventBody::InterviewCompleted(fabro_types::InterviewCompletedProps {
            question_id: question_id.clone(),
            question:    question.clone(),
            answer:      answer.clone(),
            duration_ms: *duration_ms,
        }),
        Event::InterviewTimeout {
            question_id,
            question,
            stage,
            duration_ms,
        } => EventBody::InterviewTimeout(fabro_types::InterviewTimeoutProps {
            question_id: question_id.clone(),
            question:    question.clone(),
            stage:       stage.clone(),
            duration_ms: *duration_ms,
        }),
        Event::InterviewInterrupted {
            question_id,
            question,
            stage,
            reason,
            duration_ms,
        } => EventBody::InterviewInterrupted(fabro_types::InterviewInterruptedProps {
            question_id: question_id.clone(),
            question:    question.clone(),
            stage:       stage.clone(),
            reason:      reason.clone(),
            duration_ms: *duration_ms,
        }),
        Event::CheckpointCompleted {
            status,
            current_node,
            completed_nodes,
            node_retries,
            context_values,
            node_outcomes,
            next_node_id,
            git_commit_sha,
            loop_failure_signatures,
            restart_failure_signatures,
            node_visits,
            diff,
            ..
        } => EventBody::CheckpointCompleted(fabro_types::CheckpointCompletedProps {
            status: status.clone(),
            current_node: current_node.clone(),
            completed_nodes: completed_nodes.clone(),
            node_retries: node_retries.clone(),
            context_values: context_values.clone(),
            node_outcomes: node_outcomes.clone(),
            next_node_id: next_node_id.clone(),
            git_commit_sha: git_commit_sha.clone(),
            loop_failure_signatures: loop_failure_signatures.clone(),
            restart_failure_signatures: restart_failure_signatures.clone(),
            node_visits: node_visits.clone(),
            diff: diff.clone(),
        }),
        Event::CheckpointFailed { error, .. } => {
            EventBody::CheckpointFailed(fabro_types::CheckpointFailedProps {
                error: error.clone(),
            })
        }
        Event::GitCommit { sha, .. } => {
            EventBody::GitCommit(fabro_types::GitCommitProps { sha: sha.clone() })
        }
        Event::GitPush { branch, success } => EventBody::GitPush(fabro_types::GitPushProps {
            branch:  branch.clone(),
            success: *success,
        }),
        Event::GitBranch { branch, sha } => EventBody::GitBranch(fabro_types::GitBranchProps {
            branch: branch.clone(),
            sha:    sha.clone(),
        }),
        Event::GitWorktreeAdd { path, branch } => {
            EventBody::GitWorktreeAdd(fabro_types::GitWorktreeAddProps {
                path:   path.clone(),
                branch: branch.clone(),
            })
        }
        Event::GitWorktreeRemove { path } => {
            EventBody::GitWorktreeRemove(fabro_types::GitWorktreeRemoveProps { path: path.clone() })
        }
        Event::GitFetch { branch, success } => EventBody::GitFetch(fabro_types::GitFetchProps {
            branch:  branch.clone(),
            success: *success,
        }),
        Event::GitReset { sha } => {
            EventBody::GitReset(fabro_types::GitResetProps { sha: sha.clone() })
        }
        Event::EdgeSelected {
            from_node,
            to_node,
            label,
            condition,
            reason,
            preferred_label,
            suggested_next_ids,
            stage_status,
            is_jump,
        } => EventBody::EdgeSelected(fabro_types::EdgeSelectedProps {
            from_node:          from_node.clone(),
            to_node:            to_node.clone(),
            label:              label.clone(),
            condition:          condition.clone(),
            reason:             reason.clone(),
            preferred_label:    preferred_label.clone(),
            suggested_next_ids: suggested_next_ids.clone(),
            stage_status:       stage_status.clone(),
            is_jump:            *is_jump,
        }),
        Event::LoopRestart { from_node, to_node } => {
            EventBody::LoopRestart(fabro_types::LoopRestartProps {
                from_node: from_node.clone(),
                to_node:   to_node.clone(),
            })
        }
        Event::Prompt {
            visit,
            text,
            mode,
            provider,
            model,
            ..
        } => EventBody::StagePrompt(fabro_types::StagePromptProps {
            visit:    *visit,
            text:     text.clone(),
            mode:     mode.clone(),
            provider: provider.clone(),
            model:    model.clone(),
        }),
        Event::PromptCompleted {
            response,
            model,
            provider,
            billing,
            ..
        } => EventBody::PromptCompleted(fabro_types::PromptCompletedProps {
            response: response.clone(),
            model:    model.clone(),
            provider: provider.clone(),
            billing:  billing.clone(),
        }),
        Event::Agent { visit, event, .. } => match event {
            AgentEvent::SessionStarted { provider, model } => {
                EventBody::AgentSessionStarted(fabro_types::AgentSessionStartedProps {
                    provider: provider.clone(),
                    model:    model.clone(),
                    visit:    *visit,
                })
            }
            AgentEvent::SessionEnded => {
                EventBody::AgentSessionEnded(fabro_types::AgentSessionEndedProps { visit: *visit })
            }
            AgentEvent::ProcessingEnd => {
                EventBody::AgentProcessingEnd(fabro_types::AgentProcessingEndProps {
                    visit: *visit,
                })
            }
            AgentEvent::UserInput { text } => EventBody::AgentInput(fabro_types::AgentInputProps {
                text:  text.clone(),
                visit: *visit,
            }),
            AgentEvent::AssistantMessage {
                text,
                model,
                usage,
                tool_call_count,
            } => EventBody::AgentMessage(fabro_types::AgentMessageProps {
                text:            text.clone(),
                model:           model.clone(),
                billing:         billed_token_counts_from_llm(usage),
                tool_call_count: *tool_call_count,
                visit:           *visit,
            }),
            AgentEvent::ToolCallStarted {
                tool_name,
                tool_call_id,
                arguments,
            } => EventBody::AgentToolStarted(fabro_types::AgentToolStartedProps {
                tool_name:    tool_name.clone(),
                tool_call_id: tool_call_id.clone(),
                arguments:    arguments.clone(),
                visit:        *visit,
            }),
            AgentEvent::ToolCallCompleted {
                tool_name,
                tool_call_id,
                output,
                is_error,
            } => EventBody::AgentToolCompleted(fabro_types::AgentToolCompletedProps {
                tool_name:    tool_name.clone(),
                tool_call_id: tool_call_id.clone(),
                output:       output.clone(),
                is_error:     *is_error,
                visit:        *visit,
            }),
            AgentEvent::Error { error } => EventBody::AgentError(fabro_types::AgentErrorProps {
                error: serde_json::to_value(error).expect("serializable agent error"),
                visit: *visit,
            }),
            AgentEvent::Warning {
                kind,
                message,
                details,
            } => EventBody::AgentWarning(fabro_types::AgentWarningProps {
                kind:    kind.clone(),
                message: message.clone(),
                details: details.clone(),
                visit:   *visit,
            }),
            AgentEvent::LoopDetected => {
                EventBody::AgentLoopDetected(fabro_types::AgentLoopDetectedProps { visit: *visit })
            }
            AgentEvent::TurnLimitReached { max_turns } => {
                EventBody::AgentTurnLimitReached(fabro_types::AgentTurnLimitReachedProps {
                    max_turns: *max_turns,
                    visit:     *visit,
                })
            }
            AgentEvent::SteeringInjected { text } => {
                EventBody::AgentSteeringInjected(fabro_types::AgentSteeringInjectedProps {
                    text:  text.clone(),
                    visit: *visit,
                })
            }
            AgentEvent::CompactionStarted {
                estimated_tokens,
                context_window_size,
            } => EventBody::AgentCompactionStarted(fabro_types::AgentCompactionStartedProps {
                estimated_tokens:    *estimated_tokens,
                context_window_size: *context_window_size,
                visit:               *visit,
            }),
            AgentEvent::CompactionCompleted {
                original_turn_count,
                preserved_turn_count,
                summary_token_estimate,
                tracked_file_count,
            } => EventBody::AgentCompactionCompleted(fabro_types::AgentCompactionCompletedProps {
                original_turn_count:    *original_turn_count,
                preserved_turn_count:   *preserved_turn_count,
                summary_token_estimate: *summary_token_estimate,
                tracked_file_count:     *tracked_file_count,
                visit:                  *visit,
            }),
            AgentEvent::LlmRetry {
                provider,
                model,
                attempt,
                delay_secs,
                error,
            } => EventBody::AgentLlmRetry(fabro_types::AgentLlmRetryProps {
                provider:   provider.clone(),
                model:      model.clone(),
                attempt:    *attempt,
                delay_secs: *delay_secs,
                error:      serde_json::to_value(error).expect("serializable sdk error"),
                visit:      *visit,
            }),
            AgentEvent::SubAgentSpawned {
                agent_id,
                depth,
                task,
            } => EventBody::AgentSubSpawned(fabro_types::AgentSubSpawnedProps {
                agent_id: agent_id.clone(),
                depth:    *depth,
                task:     task.clone(),
                visit:    *visit,
            }),
            AgentEvent::SubAgentCompleted {
                agent_id,
                depth,
                success,
                turns_used,
            } => EventBody::AgentSubCompleted(fabro_types::AgentSubCompletedProps {
                agent_id:   agent_id.clone(),
                depth:      *depth,
                success:    *success,
                turns_used: *turns_used,
                visit:      *visit,
            }),
            AgentEvent::SubAgentFailed {
                agent_id,
                depth,
                error,
            } => EventBody::AgentSubFailed(fabro_types::AgentSubFailedProps {
                agent_id: agent_id.clone(),
                depth:    *depth,
                error:    serde_json::to_value(error).expect("serializable agent error"),
                visit:    *visit,
            }),
            AgentEvent::SubAgentClosed { agent_id, depth } => {
                EventBody::AgentSubClosed(fabro_types::AgentSubClosedProps {
                    agent_id: agent_id.clone(),
                    depth:    *depth,
                    visit:    *visit,
                })
            }
            AgentEvent::McpServerReady {
                server_name,
                tool_count,
            } => EventBody::AgentMcpReady(fabro_types::AgentMcpReadyProps {
                server_name: server_name.clone(),
                tool_count:  *tool_count,
                visit:       *visit,
            }),
            AgentEvent::McpServerFailed { server_name, error } => {
                EventBody::AgentMcpFailed(fabro_types::AgentMcpFailedProps {
                    server_name: server_name.clone(),
                    error:       error.clone(),
                    visit:       *visit,
                })
            }
            AgentEvent::AssistantTextStart
            | AgentEvent::AssistantOutputReplace { .. }
            | AgentEvent::TextDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::ToolCallOutputDelta { .. }
            | AgentEvent::SkillExpanded { .. } => {
                panic!("streaming-noise agent event should not be converted to RunEvent")
            }
        },
        Event::SubgraphStarted { start_node, .. } => {
            EventBody::SubgraphStarted(fabro_types::SubgraphStartedProps {
                start_node: start_node.clone(),
            })
        }
        Event::SubgraphCompleted {
            steps_executed,
            status,
            duration_ms,
            ..
        } => EventBody::SubgraphCompleted(fabro_types::SubgraphCompletedProps {
            steps_executed: *steps_executed,
            status:         status.clone(),
            duration_ms:    *duration_ms,
        }),
        Event::Sandbox { event } => match event {
            SandboxEvent::Initializing { provider } => {
                EventBody::SandboxInitializing(fabro_types::SandboxInitializingProps {
                    provider: provider.clone(),
                })
            }
            SandboxEvent::Ready {
                provider,
                duration_ms,
                name,
                cpu,
                memory,
                url,
            } => EventBody::SandboxReady(fabro_types::SandboxReadyProps {
                provider:    provider.clone(),
                duration_ms: *duration_ms,
                name:        name.clone(),
                cpu:         *cpu,
                memory:      *memory,
                url:         url.clone(),
            }),
            SandboxEvent::InitializeFailed {
                provider,
                error,
                duration_ms,
            } => EventBody::SandboxFailed(fabro_types::SandboxFailedProps {
                provider:    provider.clone(),
                error:       error.clone(),
                duration_ms: *duration_ms,
            }),
            SandboxEvent::CleanupStarted { provider } => {
                EventBody::SandboxCleanupStarted(fabro_types::SandboxCleanupStartedProps {
                    provider: provider.clone(),
                })
            }
            SandboxEvent::CleanupCompleted {
                provider,
                duration_ms,
            } => EventBody::SandboxCleanupCompleted(fabro_types::SandboxCleanupCompletedProps {
                provider:    provider.clone(),
                duration_ms: *duration_ms,
            }),
            SandboxEvent::CleanupFailed { provider, error } => {
                EventBody::SandboxCleanupFailed(fabro_types::SandboxCleanupFailedProps {
                    provider: provider.clone(),
                    error:    error.clone(),
                })
            }
            SandboxEvent::SnapshotPulling { name } => {
                EventBody::SnapshotPulling(fabro_types::SnapshotNameProps { name: name.clone() })
            }
            SandboxEvent::SnapshotPulled { name, duration_ms } => {
                EventBody::SnapshotPulled(fabro_types::SnapshotCompletedProps {
                    name:        name.clone(),
                    duration_ms: *duration_ms,
                })
            }
            SandboxEvent::SnapshotEnsuring { name } => {
                EventBody::SnapshotEnsuring(fabro_types::SnapshotNameProps { name: name.clone() })
            }
            SandboxEvent::SnapshotCreating { name } => {
                EventBody::SnapshotCreating(fabro_types::SnapshotNameProps { name: name.clone() })
            }
            SandboxEvent::SnapshotReady { name, duration_ms } => {
                EventBody::SnapshotReady(fabro_types::SnapshotCompletedProps {
                    name:        name.clone(),
                    duration_ms: *duration_ms,
                })
            }
            SandboxEvent::SnapshotFailed { name, error } => {
                EventBody::SnapshotFailed(fabro_types::SnapshotFailedProps {
                    name:  name.clone(),
                    error: error.clone(),
                })
            }
            SandboxEvent::GitCloneStarted { url, branch } => {
                EventBody::GitCloneStarted(fabro_types::GitCloneStartedProps {
                    url:    url.clone(),
                    branch: branch.clone(),
                })
            }
            SandboxEvent::GitCloneCompleted { url, duration_ms } => {
                EventBody::GitCloneCompleted(fabro_types::GitCloneCompletedProps {
                    url:         url.clone(),
                    duration_ms: *duration_ms,
                })
            }
            SandboxEvent::GitCloneFailed { url, error } => {
                EventBody::GitCloneFailed(fabro_types::GitCloneFailedProps {
                    url:   url.clone(),
                    error: error.clone(),
                })
            }
        },
        Event::SandboxInitialized {
            working_directory,
            provider,
            identifier,
            host_working_directory,
            container_mount_point,
        } => EventBody::SandboxInitialized(fabro_types::SandboxInitializedProps {
            working_directory:      working_directory.clone(),
            provider:               provider.clone(),
            identifier:             identifier.clone(),
            host_working_directory: host_working_directory.clone(),
            container_mount_point:  container_mount_point.clone(),
        }),
        Event::SetupStarted { command_count } => {
            EventBody::SetupStarted(fabro_types::SetupStartedProps {
                command_count: *command_count,
            })
        }
        Event::SetupCommandStarted { command, index } => {
            EventBody::SetupCommandStarted(fabro_types::SetupCommandStartedProps {
                command: command.clone(),
                index:   *index,
            })
        }
        Event::SetupCommandCompleted {
            command,
            index,
            exit_code,
            duration_ms,
        } => EventBody::SetupCommandCompleted(fabro_types::SetupCommandCompletedProps {
            command:     command.clone(),
            index:       *index,
            exit_code:   *exit_code,
            duration_ms: *duration_ms,
        }),
        Event::SetupCompleted { duration_ms } => {
            EventBody::SetupCompleted(fabro_types::SetupCompletedProps {
                duration_ms: *duration_ms,
            })
        }
        Event::SetupFailed {
            command,
            index,
            exit_code,
            stderr,
        } => EventBody::SetupFailed(fabro_types::SetupFailedProps {
            command:   command.clone(),
            index:     *index,
            exit_code: *exit_code,
            stderr:    stderr.clone(),
        }),
        Event::StallWatchdogTimeout { idle_seconds, .. } => {
            EventBody::StallWatchdogTimeout(fabro_types::StallWatchdogTimeoutProps {
                idle_seconds: *idle_seconds,
            })
        }
        Event::ArtifactCaptured {
            attempt,
            node_slug,
            path,
            mime,
            content_md5,
            content_sha256,
            bytes,
            ..
        } => EventBody::ArtifactCaptured(fabro_types::ArtifactCapturedProps {
            attempt:        *attempt,
            node_slug:      node_slug.clone(),
            path:           path.clone(),
            mime:           mime.clone(),
            content_md5:    content_md5.clone(),
            content_sha256: content_sha256.clone(),
            bytes:          *bytes,
        }),
        Event::SshAccessReady { ssh_command } => {
            EventBody::SshAccessReady(fabro_types::SshAccessReadyProps {
                ssh_command: ssh_command.clone(),
            })
        }
        Event::Failover {
            from_provider,
            from_model,
            to_provider,
            to_model,
            error,
            ..
        } => EventBody::Failover(fabro_types::FailoverProps {
            from_provider: from_provider.clone(),
            from_model:    from_model.clone(),
            to_provider:   to_provider.clone(),
            to_model:      to_model.clone(),
            error:         error.clone(),
        }),
        Event::CliEnsureStarted { cli_name, provider } => {
            EventBody::CliEnsureStarted(fabro_types::CliEnsureStartedProps {
                cli_name: cli_name.clone(),
                provider: provider.clone(),
            })
        }
        Event::CliEnsureCompleted {
            cli_name,
            provider,
            already_installed,
            node_installed,
            duration_ms,
        } => EventBody::CliEnsureCompleted(fabro_types::CliEnsureCompletedProps {
            cli_name:          cli_name.clone(),
            provider:          provider.clone(),
            already_installed: *already_installed,
            node_installed:    *node_installed,
            duration_ms:       *duration_ms,
        }),
        Event::CliEnsureFailed {
            cli_name,
            provider,
            error,
            duration_ms,
        } => EventBody::CliEnsureFailed(fabro_types::CliEnsureFailedProps {
            cli_name:    cli_name.clone(),
            provider:    provider.clone(),
            error:       error.clone(),
            duration_ms: *duration_ms,
        }),
        Event::CommandStarted {
            script,
            command,
            language,
            timeout_ms,
            ..
        } => EventBody::CommandStarted(fabro_types::CommandStartedProps {
            script:     script.clone(),
            command:    command.clone(),
            language:   language.clone(),
            timeout_ms: *timeout_ms,
        }),
        Event::CommandCompleted {
            stdout,
            stderr,
            exit_code,
            duration_ms,
            timed_out,
            ..
        } => EventBody::CommandCompleted(fabro_types::CommandCompletedProps {
            stdout:      stdout.clone(),
            stderr:      stderr.clone(),
            exit_code:   *exit_code,
            duration_ms: *duration_ms,
            timed_out:   *timed_out,
        }),
        Event::AgentCliStarted {
            visit,
            mode,
            provider,
            model,
            command,
            ..
        } => EventBody::AgentCliStarted(fabro_types::AgentCliStartedProps {
            visit:    *visit,
            mode:     mode.clone(),
            provider: provider.clone(),
            model:    model.clone(),
            command:  command.clone(),
        }),
        Event::AgentCliCompleted {
            stdout,
            stderr,
            exit_code,
            duration_ms,
            ..
        } => EventBody::AgentCliCompleted(fabro_types::AgentCliCompletedProps {
            stdout:      stdout.clone(),
            stderr:      stderr.clone(),
            exit_code:   *exit_code,
            duration_ms: *duration_ms,
        }),
        Event::PullRequestCreated {
            pr_url,
            pr_number,
            owner,
            repo,
            base_branch,
            head_branch,
            title,
            draft,
        } => EventBody::PullRequestCreated(fabro_types::PullRequestCreatedProps {
            pr_url:      pr_url.clone(),
            pr_number:   *pr_number,
            owner:       owner.clone(),
            repo:        repo.clone(),
            base_branch: base_branch.clone(),
            head_branch: head_branch.clone(),
            title:       title.clone(),
            draft:       *draft,
        }),
        Event::PullRequestFailed { error } => {
            EventBody::PullRequestFailed(fabro_types::PullRequestFailedProps {
                error: error.clone(),
            })
        }
        Event::DevcontainerResolved {
            dockerfile_lines,
            environment_count,
            lifecycle_command_count,
            workspace_folder,
        } => EventBody::DevcontainerResolved(fabro_types::DevcontainerResolvedProps {
            dockerfile_lines:        *dockerfile_lines,
            environment_count:       *environment_count,
            lifecycle_command_count: *lifecycle_command_count,
            workspace_folder:        workspace_folder.clone(),
        }),
        Event::DevcontainerLifecycleStarted {
            phase,
            command_count,
        } => EventBody::DevcontainerLifecycleStarted(
            fabro_types::DevcontainerLifecycleStartedProps {
                phase:         phase.clone(),
                command_count: *command_count,
            },
        ),
        Event::DevcontainerLifecycleCommandStarted {
            phase,
            command,
            index,
        } => EventBody::DevcontainerLifecycleCommandStarted(
            fabro_types::DevcontainerLifecycleCommandStartedProps {
                phase:   phase.clone(),
                command: command.clone(),
                index:   *index,
            },
        ),
        Event::DevcontainerLifecycleCommandCompleted {
            phase,
            command,
            index,
            exit_code,
            duration_ms,
        } => EventBody::DevcontainerLifecycleCommandCompleted(
            fabro_types::DevcontainerLifecycleCommandCompletedProps {
                phase:       phase.clone(),
                command:     command.clone(),
                index:       *index,
                exit_code:   *exit_code,
                duration_ms: *duration_ms,
            },
        ),
        Event::DevcontainerLifecycleCompleted { phase, duration_ms } => {
            EventBody::DevcontainerLifecycleCompleted(
                fabro_types::DevcontainerLifecycleCompletedProps {
                    phase:       phase.clone(),
                    duration_ms: *duration_ms,
                },
            )
        }
        Event::DevcontainerLifecycleFailed {
            phase,
            command,
            index,
            exit_code,
            stderr,
        } => {
            EventBody::DevcontainerLifecycleFailed(fabro_types::DevcontainerLifecycleFailedProps {
                phase:     phase.clone(),
                command:   command.clone(),
                index:     *index,
                exit_code: *exit_code,
                stderr:    stderr.clone(),
            })
        }
        Event::RetroStarted {
            prompt,
            provider,
            model,
        } => EventBody::RetroStarted(fabro_types::RetroStartedProps {
            prompt:   prompt.clone(),
            provider: provider.clone(),
            model:    model.clone(),
        }),
        Event::RetroCompleted {
            duration_ms,
            response,
            retro,
        } => EventBody::RetroCompleted(fabro_types::RetroCompletedProps {
            duration_ms: *duration_ms,
            response:    response.clone(),
            retro:       retro.clone(),
        }),
        Event::RetroFailed { error, duration_ms } => {
            EventBody::RetroFailed(fabro_types::RetroFailedProps {
                error:       error.clone(),
                duration_ms: *duration_ms,
            })
        }
    }
}

/// Stage-level scope threaded through event emission to populate
/// `stage_id` / `parallel_group_id` / `parallel_branch_id` on events
/// that happen inside a concrete stage execution.
#[derive(Clone, Debug)]
pub struct StageScope {
    pub node_id:            String,
    pub visit:              u32,
    pub parallel_group_id:  Option<StageId>,
    pub parallel_branch_id: Option<ParallelBranchId>,
}

impl StageScope {
    /// Build a scope from the given node id, sourcing visit count and parallel
    /// ids from the current context.
    pub fn from_context(context: &WfContext, node_id: impl Into<String>) -> Self {
        Self {
            node_id:            node_id.into(),
            visit:              u32::try_from(visit_from_context(context)).unwrap_or(u32::MAX),
            parallel_group_id:  context.parallel_group_id(),
            parallel_branch_id: context.parallel_branch_id(),
        }
    }

    /// Build scope for a handler invocation. Prefers the `current_stage_scope`
    /// seeded by the fidelity lifecycle `before_node` hook, and falls back to
    /// synthesizing one from `node_id` for direct-handler call sites (tests,
    /// etc.) that don't go through the full lifecycle.
    pub fn for_handler(context: &WfContext, node_id: impl Into<String>) -> Self {
        context
            .current_stage_scope()
            .unwrap_or_else(|| Self::from_context(context, node_id))
    }

    /// Build scope for the branch-lifecycle events emitted by the parallel
    /// handler (`ParallelBranchStarted`, `ParallelBranchCompleted`, and the
    /// pre-dispatch `GitCommit` for the branch worktree).
    ///
    /// `target_visit` is the visit count of `target_node_id` for this
    /// particular branch dispatch. The parallel handler currently passes
    /// `1` because branches haven't been re-entered yet at the point of
    /// scope construction; a future change that loops a parallel node
    /// must pass the actual visit so envelope `stage_id`s stay accurate.
    #[must_use]
    pub fn for_parallel_branch(
        target_node_id: impl Into<String>,
        target_visit: u32,
        parallel_group_id: StageId,
        parallel_branch_id: ParallelBranchId,
    ) -> Self {
        Self {
            node_id:            target_node_id.into(),
            visit:              target_visit,
            parallel_group_id:  Some(parallel_group_id),
            parallel_branch_id: Some(parallel_branch_id),
        }
    }
}

#[must_use]
pub fn to_run_event(run_id: &RunId, event: &Event) -> RunEvent {
    to_run_event_at(run_id, event, Utc::now(), None)
}

#[must_use]
pub fn to_run_event_at(
    run_id: &RunId,
    event: &Event,
    ts: chrono::DateTime<Utc>,
    scope: Option<&StageScope>,
) -> RunEvent {
    let fields = stored_event_fields(event, scope);
    let body = event_body_from_event(event);
    RunEvent {
        id: Uuid::now_v7().to_string(),
        ts,
        run_id: *run_id,
        node_id: fields.node_id,
        node_label: fields.node_label,
        stage_id: fields.stage_id,
        parallel_group_id: fields.parallel_group_id,
        parallel_branch_id: fields.parallel_branch_id,
        session_id: fields.session_id,
        parent_session_id: fields.parent_session_id,
        tool_call_id: fields.tool_call_id,
        actor: fields.actor,
        body,
    }
}

pub fn build_redacted_event_payload(event: &RunEvent, run_id: &RunId) -> Result<EventPayload> {
    let value = redacted_event_value(event)?;
    EventPayload::new(value, run_id).map_err(anyhow::Error::from)
}

pub fn redacted_event_json(event: &RunEvent) -> Result<String> {
    serde_json::to_string(&redacted_event_value(event)?).map_err(anyhow::Error::from)
}

fn normalized_event_value(event: &RunEvent) -> Result<Value> {
    let value = event.to_value()?;
    Ok(normalize_json_value(value))
}

fn redacted_event_value(event: &RunEvent) -> Result<Value> {
    Ok(redact_json_value(normalized_event_value(event)?))
}

pub fn event_payload_from_redacted_json(line: &str, run_id: &RunId) -> Result<EventPayload> {
    let value = serde_json::from_str(line).context("Failed to parse redacted event payload")?;
    EventPayload::new(value, run_id).map_err(anyhow::Error::from)
}

pub async fn append_event(run_store: &RunDatabase, run_id: &RunId, event: &Event) -> Result<()> {
    let stored = to_run_event(run_id, event);
    let payload = build_redacted_event_payload(&stored, run_id)?;
    run_store
        .append_event(&payload)
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

pub async fn append_event_to_sink(
    sink: &RunEventSink,
    run_id: &RunId,
    event: &Event,
) -> Result<()> {
    let stored = to_run_event(run_id, event);
    sink.write_run_event(&stored).await
}

#[derive(Clone)]
pub enum RunEventSink {
    Store(RunStoreHandle),
    JsonLines(Arc<AsyncMutex<Pin<Box<dyn AsyncWrite + Send>>>>),
    Callback(Arc<RunEventSinkCallback>),
    Composite(Vec<Self>),
}

type RunEventSinkFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
type RunEventSinkCallback = dyn Fn(RunEvent) -> RunEventSinkFuture + Send + Sync + 'static;

impl RunEventSink {
    #[must_use]
    pub fn store(run_store: RunDatabase) -> Self {
        Self::Store(RunStoreHandle::local(run_store))
    }

    #[must_use]
    pub fn backend(run_store: RunStoreHandle) -> Self {
        Self::Store(run_store)
    }

    #[must_use]
    pub fn json_lines<W>(writer: W) -> Self
    where
        W: AsyncWrite + Send + 'static,
    {
        Self::JsonLines(Arc::new(AsyncMutex::new(Box::pin(writer))))
    }

    #[must_use]
    pub fn callback<F, Fut>(callback: F) -> Self
    where
        F: Fn(RunEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self::Callback(Arc::new(move |event| Box::pin(callback(event))))
    }

    #[must_use]
    pub fn fanout(sinks: Vec<Self>) -> Self {
        let mut flattened = Vec::new();
        for sink in sinks {
            match sink {
                Self::Composite(inner) => flattened.extend(inner),
                other => flattened.push(other),
            }
        }
        Self::Composite(flattened)
    }

    pub async fn write_run_event(&self, event: &RunEvent) -> Result<()> {
        let mut pending = vec![self];
        while let Some(sink) = pending.pop() {
            match sink {
                Self::Store(run_store) => {
                    run_store.append_run_event(event).await?;
                }
                Self::JsonLines(writer) => {
                    let line = redacted_event_json(event)?;
                    let mut writer = writer.lock().await;
                    writer.write_all(line.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                }
                Self::Callback(callback) => callback(event.clone()).await?,
                Self::Composite(sinks) => {
                    pending.extend(sinks.iter().rev());
                }
            }
        }
        Ok(())
    }
}

#[allow(clippy::large_enum_variant)]
enum RunEventCommand {
    Event(RunEvent),
    Flush(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct RunEventLogger {
    tx: mpsc::UnboundedSender<RunEventCommand>,
}

impl RunEventLogger {
    #[must_use]
    pub fn new(sink: RunEventSink) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    RunEventCommand::Event(event) => {
                        if let Err(err) = sink.write_run_event(&event).await {
                            tracing::warn!(error = %err, "Failed to write run event");
                        }
                    }
                    RunEventCommand::Flush(tx) => {
                        let _ = tx.send(());
                    }
                }
            }
        });

        Self { tx }
    }

    pub fn register(&self, emitter: &Emitter) {
        let tx = self.tx.clone();
        emitter.on_event(move |event| {
            if tx.send(RunEventCommand::Event(event.clone())).is_err() {
                tracing::warn!("Run event logger channel closed while forwarding event");
            }
        });
    }

    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(RunEventCommand::Flush(tx)).is_err() {
            tracing::warn!("Run event logger channel closed before flush");
            return;
        }
        if rx.await.is_err() {
            tracing::warn!("Run event logger flush dropped before completion");
        }
    }
}

#[derive(Clone)]
pub struct StoreProgressLogger {
    inner: RunEventLogger,
}

impl StoreProgressLogger {
    #[must_use]
    pub fn new(run_store: impl Into<RunStoreHandle>) -> Self {
        Self {
            inner: RunEventLogger::new(RunEventSink::backend(run_store.into())),
        }
    }

    pub fn register(&self, emitter: &Emitter) {
        self.inner.register(emitter);
    }

    pub async fn flush(&self) {
        self.inner.flush().await;
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
type EventListener = Arc<dyn Fn(&RunEvent) + Send + Sync>;

/// Callback-based event emitter for workflow run events.
pub struct Emitter {
    run_id:        RunId,
    listeners:     std::sync::Mutex<Vec<EventListener>>,
    /// Epoch milliseconds of the last `emit()` or `touch()` call. 0 until first
    /// event.
    last_event_at: AtomicI64,
}

impl std::fmt::Debug for Emitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.listeners.lock().map(|l| l.len()).unwrap_or(0);
        f.debug_struct("Emitter")
            .field("run_id", &self.run_id)
            .field("listener_count", &count)
            .field("last_event_at", &self.last_event_at.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new(RunId::new())
    }
}

impl Emitter {
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

    pub fn on_event(&self, listener: impl Fn(&RunEvent) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .expect("listeners lock poisoned")
            .push(Arc::new(listener));
    }

    pub fn emit(&self, event: &Event) {
        self.emit_with_scope(event, None);
    }

    pub fn emit_scoped(&self, event: &Event, scope: &StageScope) {
        self.emit_with_scope(event, Some(scope));
    }

    fn emit_with_scope(&self, event: &Event, scope: Option<&StageScope>) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
        event.trace();
        if let Event::WorkflowRunStarted { run_id, .. } = event {
            debug_assert_eq!(
                *run_id, self.run_id,
                "workflow run started event must match emitter run_id"
            );
        }
        let stored = to_run_event_at(&self.run_id, event, Utc::now(), scope);
        self.dispatch_run_event(&stored);
    }

    pub(crate) fn dispatch_run_event(&self, event: &RunEvent) {
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

    /// Manually update the last-event timestamp (e.g. to seed the watchdog at
    /// workflow run start).
    pub fn touch(&self) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
    }

    /// Build a [`WorktreeEventCallback`] that forwards worktree lifecycle
    /// events as [`Event`]s on this emitter.
    pub fn worktree_callback(self: Arc<Self>) -> WorktreeEventCallback {
        Arc::new(move |event| match event {
            WorktreeEvent::BranchCreated { branch, sha } => {
                self.emit(&Event::GitBranch { branch, sha });
            }
            WorktreeEvent::WorktreeAdded { path, branch } => {
                self.emit(&Event::GitWorktreeAdd { path, branch });
            }
            WorktreeEvent::WorktreeRemoved { path } => {
                self.emit(&Event::GitWorktreeRemove { path });
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ::fabro_types::{ActorKind, fixtures};

    use super::*;

    #[test]
    fn event_emitter_new_has_no_listeners() {
        let emitter = Emitter::new(fixtures::RUN_1);
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }

    #[test]
    fn event_emitter_calls_listener_with_envelope() {
        let emitter = Emitter::new(fixtures::RUN_1);
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        emitter.on_event(move |event| {
            received_clone.lock().unwrap().push(event.clone());
        });
        emitter.emit(&Event::WorkflowRunStarted {
            name:         "test".to_string(),
            run_id:       fixtures::RUN_1,
            base_branch:  None,
            base_sha:     None,
            run_branch:   None,
            worktree_dir: None,
            goal:         None,
        });
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name(), "run.started");
        assert_eq!(events[0].run_id, fixtures::RUN_1);
        assert!(events[0].id.len() >= 32);
    }

    #[test]
    fn event_emitter_default() {
        let emitter = Emitter::default();
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }

    #[test]
    fn run_event_stage_completed_places_node_fields_in_header() {
        let stored = to_run_event_at(
            &fixtures::RUN_2,
            &Event::StageCompleted {
                node_id: "plan".to_string(),
                name: "Plan".to_string(),
                index: 0,
                duration_ms: 5000,
                status: "success".to_string(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                billing: None,
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
            Utc::now(),
            Some(&StageScope {
                node_id:            "plan".to_string(),
                visit:              1,
                parallel_group_id:  None,
                parallel_branch_id: None,
            }),
        );

        assert_eq!(stored.event_name(), "stage.completed");
        assert_eq!(stored.run_id, fixtures::RUN_2);
        assert_eq!(stored.node_id.as_deref(), Some("plan"));
        assert_eq!(stored.node_label.as_deref(), Some("Plan"));
        assert_eq!(stored.stage_id, Some(StageId::new("plan", 1)));
        let properties = stored.properties().unwrap();
        assert_eq!(properties["duration_ms"], 5000);
        assert_eq!(properties["status"], "success");
        assert!(stored.session_id.is_none());
    }

    #[test]
    fn run_event_stage_completed_keeps_response_and_signature_snapshots() {
        let stored = to_run_event(&fixtures::RUN_2, &Event::StageCompleted {
            node_id: "plan".to_string(),
            name: "Plan".to_string(),
            index: 0,
            duration_ms: 5000,
            status: "success".to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing: None,
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
        });

        let properties = stored.properties().unwrap();
        assert_eq!(properties["response"], "done");
        assert_eq!(properties["loop_failure_signatures"]["sig-a"], 2);
        assert_eq!(properties["restart_failure_signatures"]["sig-b"], 1);
    }

    #[test]
    fn run_event_stage_failure_keeps_failure_detail() {
        let stored = to_run_event(&fixtures::RUN_3, &Event::StageFailed {
            node_id:    "code".to_string(),
            name:       "Code".to_string(),
            index:      1,
            failure:    FailureDetail::new(
                "lint failed",
                crate::outcome::FailureCategory::Deterministic,
            ),
            will_retry: true,
        });

        assert_eq!(stored.event_name(), "stage.failed");
        let properties = stored.properties().unwrap();
        assert_eq!(properties["failure"]["message"], "lint failed");
        assert_eq!(properties["failure"]["failure_class"], "deterministic");
        assert_eq!(properties["will_retry"], true);
    }

    #[test]
    fn run_event_agent_tool_started_moves_session_metadata_to_header() {
        let stored = to_run_event(&fixtures::RUN_4, &Event::Agent {
            stage:             "code".to_string(),
            visit:             2,
            event:             AgentEvent::ToolCallStarted {
                tool_name:    "read_file".to_string(),
                tool_call_id: "call_1".to_string(),
                arguments:    serde_json::json!({"path": "src/main.rs"}),
            },
            session_id:        Some("ses_child".to_string()),
            parent_session_id: Some("ses_parent".to_string()),
        });

        assert_eq!(stored.event_name(), "agent.tool.started");
        assert_eq!(stored.node_id.as_deref(), Some("code"));
        assert_eq!(stored.node_label.as_deref(), Some("code"));
        assert_eq!(stored.session_id.as_deref(), Some("ses_child"));
        assert_eq!(stored.parent_session_id.as_deref(), Some("ses_parent"));
        let properties = stored.properties().unwrap();
        assert_eq!(properties["tool_name"], "read_file");
        assert_eq!(properties["tool_call_id"], "call_1");
        assert_eq!(properties["visit"], 2);
    }

    #[test]
    fn run_event_sandbox_event_keeps_properties_nested() {
        let stored = to_run_event(&fixtures::RUN_5, &Event::Sandbox {
            event: SandboxEvent::Ready {
                provider:    "daytona".to_string(),
                duration_ms: 2500,
                name:        Some("sandbox-1".to_string()),
                cpu:         Some(4.0),
                memory:      Some(8.0),
                url:         Some("https://example.test".to_string()),
            },
        });

        assert_eq!(stored.event_name(), "sandbox.ready");
        assert!(stored.node_id.is_none());
        let properties = stored.properties().unwrap();
        assert_eq!(properties["provider"], "daytona");
        assert_eq!(properties["duration_ms"], 2500);
    }

    #[test]
    fn run_event_workflow_failure_uses_display_error() {
        let stored = to_run_event(&fixtures::RUN_6, &Event::WorkflowRunFailed {
            error:          Error::handler("boom"),
            duration_ms:    900,
            reason:         Some(StatusReason::WorkflowError),
            git_commit_sha: Some("abc123".to_string()),
        });

        assert_eq!(stored.event_name(), "run.failed");
        let properties = stored.properties().unwrap();
        assert_eq!(properties["error"], "Handler error: boom");
        assert_eq!(properties["duration_ms"], 900);
    }

    #[tokio::test]
    async fn append_event_writes_store_event_shape() {
        let store = fabro_store::Database::new(
            std::sync::Arc::new(object_store::memory::InMemory::new()),
            "",
            std::time::Duration::from_millis(1),
        );
        let run_store = store.create_run(&fixtures::RUN_7).await.unwrap();
        let stored = to_run_event(&fixtures::RUN_7, &Event::RunNotice {
            level:   RunNoticeLevel::Warn,
            code:    "example".to_string(),
            message: "notice".to_string(),
        });
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

    #[tokio::test]
    async fn run_event_sink_json_lines_writes_canonical_event_lines() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (writer, reader) = tokio::io::duplex(4096);
        let sink = RunEventSink::json_lines(writer);
        let event = to_run_event(&fixtures::RUN_7, &Event::RunPauseRequested { actor: None });

        sink.write_run_event(&event).await.unwrap();

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let payload = event_payload_from_redacted_json(line.trim_end(), &fixtures::RUN_7).unwrap();
        assert_eq!(payload.as_value()["event"], "run.pause.requested");
        assert_eq!(payload.as_value()["properties"]["action"], "pause");
    }

    #[tokio::test]
    async fn run_event_logger_registers_emitter_events_to_json_lines() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (writer, reader) = tokio::io::duplex(4096);
        let sink = RunEventSink::json_lines(writer);
        let logger = RunEventLogger::new(sink);
        let emitter = Emitter::new(fixtures::RUN_8);
        logger.register(&emitter);

        emitter.emit(&Event::RunPaused);
        logger.flush().await;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let payload = event_payload_from_redacted_json(line.trim_end(), &fixtures::RUN_8).unwrap();
        assert_eq!(payload.as_value()["event"], "run.paused");
    }

    #[test]
    fn build_redacted_event_payload_requires_id() {
        let stored = to_run_event(&fixtures::RUN_8, &Event::RetroStarted {
            prompt:   Some("Analyze the run".to_string()),
            provider: None,
            model:    None,
        });
        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_8).unwrap();
        assert_eq!(payload.as_value()["id"], stored.id);
        assert_eq!(payload.as_value()["event"], "retro.started");
        assert_eq!(
            payload.as_value()["properties"]["prompt"],
            "Analyze the run"
        );
    }

    #[test]
    fn event_name_matches_new_dot_notation() {
        assert_eq!(
            event_name(&Event::RetroStarted {
                prompt:   None,
                provider: None,
                model:    None,
            }),
            "retro.started"
        );
        assert_eq!(
            event_name(&Event::ParallelBranchStarted {
                parallel_group_id:  StageId::new("plan", 1),
                parallel_branch_id: ParallelBranchId::new(StageId::new("plan", 1), 0),
                branch:             "fork".to_string(),
                index:              0,
            }),
            "parallel.branch.started"
        );
        assert_eq!(
            event_name(&Event::Agent {
                stage:             "code".to_string(),
                visit:             1,
                event:             AgentEvent::SubAgentSpawned {
                    agent_id: "a1".to_string(),
                    depth:    1,
                    task:     "do it".to_string(),
                },
                session_id:        None,
                parent_session_id: None,
            }),
            "agent.sub.spawned"
        );
    }

    #[test]
    fn stage_started_populates_parallel_ids_when_present() {
        let stored = to_run_event_at(
            &fixtures::RUN_1,
            &Event::StageStarted {
                node_id:      "review".to_string(),
                name:         "review".to_string(),
                index:        1,
                handler_type: "agent".to_string(),
                attempt:      1,
                max_attempts: 1,
            },
            Utc::now(),
            Some(&StageScope {
                node_id:            "review".to_string(),
                visit:              1,
                parallel_group_id:  Some(StageId::new("fanout", 2)),
                parallel_branch_id: Some(ParallelBranchId::new(StageId::new("fanout", 2), 1)),
            }),
        );
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert_eq!(
            stored.parallel_branch_id,
            Some(ParallelBranchId::new(StageId::new("fanout", 2), 1))
        );
    }

    #[test]
    fn parallel_started_populates_parallel_group_id() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::ParallelStarted {
            node_id:      "fanout".to_string(),
            visit:        2,
            branch_count: 3,
            join_policy:  "wait_all".to_string(),
        });
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert!(stored.parallel_branch_id.is_none());
    }

    #[test]
    fn parallel_branch_started_populates_group_and_branch_ids() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::ParallelBranchStarted {
            parallel_group_id:  StageId::new("fanout", 2),
            parallel_branch_id: ParallelBranchId::new(StageId::new("fanout", 2), 1),
            branch:             "review".to_string(),
            index:              1,
        });
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert_eq!(
            stored.parallel_branch_id,
            Some(ParallelBranchId::new(StageId::new("fanout", 2), 1))
        );
    }

    #[test]
    fn agent_tool_started_populates_tool_call_id_and_stage_id() {
        let stored = to_run_event_at(
            &fixtures::RUN_1,
            &Event::Agent {
                stage:             "code".to_string(),
                visit:             3,
                event:             AgentEvent::ToolCallStarted {
                    tool_name:    "read_file".to_string(),
                    tool_call_id: "call_abc".to_string(),
                    arguments:    serde_json::json!({"path": "src/main.rs"}),
                },
                session_id:        Some("ses_1".to_string()),
                parent_session_id: None,
            },
            Utc::now(),
            Some(&StageScope {
                node_id:            "code".to_string(),
                visit:              3,
                parallel_group_id:  Some(StageId::new("fanout", 2)),
                parallel_branch_id: Some(ParallelBranchId::new(StageId::new("fanout", 2), 0)),
            }),
        );
        assert_eq!(stored.stage_id, Some(StageId::new("code", 3)));
        assert_eq!(stored.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert_eq!(
            stored.parallel_branch_id,
            Some(ParallelBranchId::new(StageId::new("fanout", 2), 0))
        );
    }

    #[test]
    fn stage_scope_populates_stage_id_on_non_stage_events() {
        // Events tied to a concrete stage execution but lacking scope in their
        // own variant fields (CheckpointCompleted, CommandStarted, PromptCompleted,
        // Prompt, InterviewStarted, Failover, GitCommit) should pick up stage_id
        // / parallel_group_id / parallel_branch_id from the scope argument.
        let scope = StageScope {
            node_id:            "build".to_string(),
            visit:              2,
            parallel_group_id:  Some(StageId::new("fanout", 1)),
            parallel_branch_id: Some(ParallelBranchId::new(StageId::new("fanout", 1), 0)),
        };

        let command_started = to_run_event_at(
            &fixtures::RUN_1,
            &Event::CommandStarted {
                node_id:    "build".to_string(),
                script:     "echo".to_string(),
                command:    "echo".to_string(),
                language:   "shell".to_string(),
                timeout_ms: None,
            },
            Utc::now(),
            Some(&scope),
        );
        assert_eq!(command_started.stage_id, Some(StageId::new("build", 2)));
        assert_eq!(command_started.parallel_group_id, scope.parallel_group_id);
        assert_eq!(command_started.parallel_branch_id, scope.parallel_branch_id);

        let prompt = to_run_event_at(
            &fixtures::RUN_1,
            &Event::Prompt {
                stage:    "build".to_string(),
                visit:    2,
                text:     "do it".to_string(),
                mode:     None,
                provider: None,
                model:    None,
            },
            Utc::now(),
            Some(&scope),
        );
        assert_eq!(prompt.stage_id, Some(StageId::new("build", 2)));

        let git_commit = to_run_event_at(
            &fixtures::RUN_1,
            &Event::GitCommit {
                node_id: Some("build".to_string()),
                sha:     "deadbeef".to_string(),
            },
            Utc::now(),
            Some(&scope),
        );
        assert_eq!(git_commit.stage_id, Some(StageId::new("build", 2)));
    }

    #[test]
    fn run_level_events_without_scope_leave_stage_id_absent() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::RunRunning { reason: None });
        assert!(stored.stage_id.is_none());
        assert!(stored.parallel_group_id.is_none());
        assert!(stored.parallel_branch_id.is_none());
    }

    #[test]
    fn control_action_events_carry_actor_in_envelope() {
        let actor = ActorRef {
            kind:    ActorKind::User,
            id:      Some("alice".to_string()),
            display: Some("alice".to_string()),
        };

        let cancel = to_run_event(&fixtures::RUN_1, &Event::RunCancelRequested {
            actor: Some(actor.clone()),
        });
        assert_eq!(cancel.event_name(), "run.cancel.requested");
        assert_eq!(cancel.actor.as_ref().expect("actor set"), &actor);

        let pause = to_run_event(&fixtures::RUN_1, &Event::RunPauseRequested {
            actor: Some(actor.clone()),
        });
        assert_eq!(pause.actor.as_ref().expect("actor set"), &actor);

        let unpause = to_run_event(&fixtures::RUN_1, &Event::RunUnpauseRequested {
            actor: None,
        });
        assert!(unpause.actor.is_none());
    }

    #[test]
    fn agent_assistant_message_populates_agent_actor() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::Agent {
            stage:             "code".to_string(),
            visit:             1,
            event:             AgentEvent::AssistantMessage {
                text:            "ok".to_string(),
                model:           "claude-sonnet".to_string(),
                usage:           LlmTokenCounts::default(),
                tool_call_count: 0,
            },
            session_id:        Some("ses_agent".to_string()),
            parent_session_id: None,
        });
        let actor = stored.actor.as_ref().expect("actor set");
        assert_eq!(actor.kind, ActorKind::Agent);
        assert_eq!(actor.id.as_deref(), Some("ses_agent"));
        assert_eq!(actor.display.as_deref(), Some("claude-sonnet"));
    }

    #[test]
    fn run_created_populates_user_actor_from_provenance() {
        use ::fabro_types::settings::SettingsLayer;
        use ::fabro_types::{Graph, RunAuthMethod, RunSubjectProvenance, fixtures};

        let provenance = RunProvenance {
            server:  None,
            client:  None,
            subject: Some(RunSubjectProvenance {
                login:       Some("alice".to_string()),
                auth_method: RunAuthMethod::Cookie,
            }),
        };

        let stored = to_run_event(&fixtures::RUN_1, &Event::RunCreated {
            run_id:            fixtures::RUN_1,
            settings:          serde_json::to_value(SettingsLayer::default()).unwrap(),
            graph:             serde_json::to_value(Graph::new("test")).unwrap(),
            workflow_source:   None,
            workflow_config:   None,
            labels:            BTreeMap::default(),
            run_dir:           "/tmp/run".to_string(),
            working_directory: "/tmp/run".to_string(),
            host_repo_path:    None,
            repo_origin_url:   None,
            base_branch:       None,
            workflow_slug:     None,
            db_prefix:         None,
            provenance:        Some(provenance),
            manifest_blob:     None,
        });
        let actor = stored.actor.as_ref().expect("actor set");
        assert_eq!(actor.kind, ActorKind::User);
        assert_eq!(actor.id.as_deref(), Some("alice"));
        assert_eq!(actor.display.as_deref(), Some("alice"));
    }
}
