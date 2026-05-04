use std::collections::BTreeMap;

use ::fabro_types::{
    BilledTokenCounts, BlockedReason, CommandTermination, FailureReason, ForkSourceRef, GitContext,
    ParallelBranchId, Principal, PullRequestRecord, RunBlobId, RunId, RunNoticeLevel,
    RunProvenance, StageId, SteerKind, SuccessReason, run_event as fabro_types,
};
use fabro_agent::{AgentEvent, SandboxEvent};
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::outcome::{BilledModelUsage, FailureDetail, Outcome};

/// Events emitted during workflow run execution for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "Workflow events stay inline to match the serialized event stream."
)]
pub enum Event {
    RunCreated {
        run_id:           RunId,
        settings:         serde_json::Value,
        graph:            serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_source:  Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_config:  Option<String>,
        labels:           BTreeMap<String, String>,
        run_dir:          String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_slug:    Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        db_prefix:        Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance:       Option<RunProvenance>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manifest_blob:    Option<RunBlobId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git:              Option<GitContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork_source_ref:  Option<ForkSourceRef>,
        #[serde(default)]
        in_place:         bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        web_url:          Option<String>,
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
        definition_blob: Option<RunBlobId>,
    },
    RunQueued,
    RunStarting,
    RunRunning,
    RunBlocked {
        blocked_reason: BlockedReason,
    },
    RunUnblocked,
    RunRemoving,
    RunCancelRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunPauseRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunUnpauseRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunPaused,
    RunUnpaused,
    RunSupersededBy {
        new_run_id:                RunId,
        target_checkpoint_ordinal: usize,
        target_node_id:            String,
        target_visit:              usize,
    },
    RunArchived {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunUnarchived {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    WorkflowRunCompleted {
        duration_ms:          u64,
        artifact_count:       usize,
        #[serde(default)]
        status:               String,
        reason:               SuccessReason,
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
        reason:         FailureReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_commit_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_patch:    Option<String>,
    },
    RunNotice {
        level:            RunNoticeLevel,
        code:             String,
        message:          String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
    },
    MetadataSnapshotStarted {
        phase:  fabro_types::MetadataSnapshotPhase,
        branch: String,
    },
    MetadataSnapshotCompleted {
        phase:       fabro_types::MetadataSnapshotPhase,
        branch:      String,
        duration_ms: u64,
        entry_count: usize,
        bytes:       u64,
        commit_sha:  String,
    },
    MetadataSnapshotFailed {
        phase:            fabro_types::MetadataSnapshotPhase,
        branch:           String,
        duration_ms:      u64,
        failure_kind:     fabro_types::MetadataSnapshotFailureKind,
        error:            String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes:           Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit_sha:       Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_count:      Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes:            Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
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
        node_id:     String,
        name:        String,
        index:       usize,
        failure:     FailureDetail,
        will_retry:  bool,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:       Option<Principal>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:       Option<Principal>,
        question_id: String,
        question:    String,
        answer:      String,
        duration_ms: u64,
    },
    InterviewTimeout {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:       Option<Principal>,
        question_id: String,
        question:    String,
        stage:       String,
        duration_ms: u64,
    },
    InterviewInterrupted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:       Option<Principal>,
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
        node_id:          String,
        error:            String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
    },
    GitCommit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        sha:     String,
    },
    GitPush {
        branch:           String,
        success:          bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
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
        working_directory: String,
        provider:          String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identifier:        Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo_cloned:       Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clone_origin_url:  Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clone_branch:      Option<String>,
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
        command:          String,
        index:            usize,
        exit_code:        i32,
        stderr:           String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
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
        cli_name:         String,
        provider:         String,
        error:            String,
        duration_ms:      u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
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
        node_id:           String,
        stdout:            String,
        stderr:            String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code:         Option<i32>,
        duration_ms:       u64,
        termination:       CommandTermination,
        stdout_bytes:      u64,
        stderr_bytes:      u64,
        streams_separated: bool,
        live_streaming:    bool,
    },
    AgentCliStarted {
        node_id:  String,
        visit:    u32,
        mode:     String,
        provider: String,
        model:    String,
        command:  String,
    },
    /// A `SteeringHub` registered an active API-mode session for a stage.
    /// Emitted once per `register` insert (not on replace).
    AgentSteeringAttached {
        node_id: String,
        visit:   u32,
    },
    /// The corresponding session was unregistered from the hub.
    AgentSteeringDetached {
        node_id: String,
        visit:   u32,
    },
    /// A steer arrived with no active session and was parked in the run-wide
    /// pending buffer. The actor (steer author) is lifted to top-level.
    AgentSteerBuffered {
        kind:  SteerKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    /// One or more buffered/queued steers were dropped because a cap was
    /// reached or the run ended before they could be delivered.
    AgentSteerDropped {
        reason:  fabro_types::AgentSteerDroppedReason,
        count:   u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:   Option<Principal>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visit:   Option<u32>,
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
        phase:            String,
        command:          String,
        index:            usize,
        exit_code:        i32,
        stderr:           String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
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
        error:            String,
        duration_ms:      u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
    },
}

impl Event {
    pub fn pull_request_created(record: &PullRequestRecord, draft: bool) -> Self {
        Self::PullRequestCreated {
            pr_url: record.html_url.clone(),
            pr_number: record.number,
            owner: record.owner.clone(),
            repo: record.repo.clone(),
            base_branch: record.base_branch.clone(),
            head_branch: record.head_branch.clone(),
            title: record.title.clone(),
            draft,
        }
    }

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
            Self::RunSubmitted { definition_blob } => {
                info!(?definition_blob, "Run submitted");
            }
            Self::RunQueued => {
                info!("Run queued");
            }
            Self::RunStarting => {
                info!("Run starting");
            }
            Self::RunRunning => {
                info!("Run running");
            }
            Self::RunBlocked { blocked_reason } => {
                info!(?blocked_reason, "Run blocked");
            }
            Self::RunUnblocked => {
                info!("Run unblocked");
            }
            Self::RunRemoving => {
                info!("Run removing");
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
            Self::RunSupersededBy {
                new_run_id,
                target_checkpoint_ordinal,
                target_node_id,
                target_visit,
            } => {
                info!(
                    %new_run_id,
                    target_checkpoint_ordinal,
                    target_node_id,
                    target_visit,
                    "Run superseded by new run"
                );
            }
            Self::RunArchived { actor } => {
                info!(?actor, "Run archived");
            }
            Self::RunUnarchived { actor } => {
                info!(?actor, "Run unarchived");
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
                error!(
                    error = %error,
                    causes = ?error.causes(),
                    duration_ms,
                    "Workflow run failed"
                );
            }
            Self::RunNotice {
                level,
                code,
                message,
                exec_output_tail,
            } => match level {
                RunNoticeLevel::Info => {
                    let tail =
                        fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                    info!(
                        code,
                        message,
                        exec_output_tail_present = tail.present,
                        exec_stdout_tail_bytes = tail.stdout_bytes,
                        exec_stderr_tail_bytes = tail.stderr_bytes,
                        exec_stdout_truncated = tail.stdout_truncated,
                        exec_stderr_truncated = tail.stderr_truncated,
                        "Run notice"
                    );
                }
                RunNoticeLevel::Warn => {
                    let tail =
                        fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                    warn!(
                        code,
                        message,
                        exec_output_tail_present = tail.present,
                        exec_stdout_tail_bytes = tail.stdout_bytes,
                        exec_stderr_tail_bytes = tail.stderr_bytes,
                        exec_stdout_truncated = tail.stdout_truncated,
                        exec_stderr_truncated = tail.stderr_truncated,
                        "Run notice"
                    );
                }
                RunNoticeLevel::Error => {
                    let tail =
                        fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                    error!(
                        code,
                        message,
                        exec_output_tail_present = tail.present,
                        exec_stdout_tail_bytes = tail.stdout_bytes,
                        exec_stderr_tail_bytes = tail.stderr_bytes,
                        exec_stdout_truncated = tail.stdout_truncated,
                        exec_stderr_truncated = tail.stderr_truncated,
                        "Run notice"
                    );
                }
            },
            Self::MetadataSnapshotStarted { phase, branch } => {
                debug!(%phase, branch, "Metadata snapshot started");
            }
            Self::MetadataSnapshotCompleted {
                phase,
                branch,
                duration_ms,
                ..
            } => {
                info!(%phase, branch, duration_ms, "Metadata snapshot completed");
            }
            Self::MetadataSnapshotFailed {
                phase,
                branch,
                duration_ms,
                failure_kind,
                error,
                exec_output_tail,
                ..
            } => {
                let tail = fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                warn!(
                    %phase,
                    branch,
                    duration_ms,
                    %failure_kind,
                    error,
                    exec_output_tail_present = tail.present,
                    exec_stdout_tail_bytes = tail.stdout_bytes,
                    exec_stderr_tail_bytes = tail.stderr_bytes,
                    exec_stdout_truncated = tail.stdout_truncated,
                    exec_stderr_truncated = tail.stderr_truncated,
                    "Metadata snapshot failed"
                );
            }
            Self::StageStarted {
                node_id,
                name,
                index,
                handler_type,
                attempt,
                max_attempts,
                ..
            } => {
                info!(
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
                info!(
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
                info!(
                    node_id,
                    status,
                    completed_count = completed_nodes.len(),
                    "Checkpoint completed"
                );
            }
            Self::CheckpointFailed {
                node_id,
                error,
                exec_output_tail,
            } => {
                let tail = fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                error!(
                    node_id,
                    error,
                    exec_output_tail_present = tail.present,
                    exec_stdout_tail_bytes = tail.stdout_bytes,
                    exec_stderr_tail_bytes = tail.stderr_bytes,
                    exec_stdout_truncated = tail.stdout_truncated,
                    exec_stderr_truncated = tail.stderr_truncated,
                    "Checkpoint failed"
                );
            }
            Self::GitCommit { node_id, sha } => {
                debug!(
                    node_id = node_id.as_deref().unwrap_or(""),
                    sha, "Git commit"
                );
            }
            Self::GitPush {
                branch,
                success,
                exec_output_tail,
            } => {
                if *success {
                    debug!(branch, "Git push succeeded");
                } else {
                    let tail =
                        fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                    warn!(
                        branch,
                        exec_output_tail_present = tail.present,
                        exec_stdout_tail_bytes = tail.stdout_bytes,
                        exec_stderr_tail_bytes = tail.stderr_bytes,
                        exec_stdout_truncated = tail.stdout_truncated,
                        exec_stderr_truncated = tail.stderr_truncated,
                        "Git push failed"
                    );
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
                info!(
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
                exec_output_tail,
                ..
            } => {
                let tail = fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                error!(
                    command,
                    index,
                    exit_code,
                    exec_output_tail_present = tail.present,
                    exec_stdout_tail_bytes = tail.stdout_bytes,
                    exec_stderr_tail_bytes = tail.stderr_bytes,
                    exec_stdout_truncated = tail.stdout_truncated,
                    exec_stderr_truncated = tail.stderr_truncated,
                    "Setup command failed"
                );
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
                exec_output_tail,
            } => {
                let tail = fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                error!(
                    cli_name,
                    provider,
                    error,
                    duration_ms,
                    exec_output_tail_present = tail.present,
                    exec_stdout_tail_bytes = tail.stdout_bytes,
                    exec_stderr_tail_bytes = tail.stderr_bytes,
                    exec_stdout_truncated = tail.stdout_truncated,
                    exec_stderr_truncated = tail.stderr_truncated,
                    "CLI ensure failed"
                );
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
                termination,
                stdout_bytes,
                stderr_bytes,
                ..
            } => {
                debug!(
                    node_id,
                    exit_code,
                    duration_ms,
                    termination = %termination,
                    stdout_bytes,
                    stderr_bytes,
                    "Command completed"
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
            Self::AgentSteeringAttached { node_id, visit } => {
                debug!(node_id, visit, "Steering hub attached to session");
            }
            Self::AgentSteeringDetached { node_id, visit } => {
                debug!(node_id, visit, "Steering hub detached from session");
            }
            Self::AgentSteerBuffered { kind, .. } => {
                debug!(kind = kind.as_str(), "Steer buffered (no active session)");
            }
            Self::AgentSteerDropped { reason, count, .. } => {
                warn!(?reason, count, "Steer dropped");
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
                exec_output_tail,
                ..
            } => {
                let tail = fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                error!(
                    phase,
                    command,
                    index,
                    exit_code,
                    exec_output_tail_present = tail.present,
                    exec_stdout_tail_bytes = tail.stdout_bytes,
                    exec_stderr_tail_bytes = tail.stderr_bytes,
                    exec_stdout_truncated = tail.stdout_truncated,
                    exec_stderr_truncated = tail.stderr_truncated,
                    "Devcontainer lifecycle command failed"
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
            Self::RetroFailed {
                error,
                duration_ms,
                exec_output_tail,
            } => {
                let tail = fabro_types::ExecOutputTail::trace_summary(exec_output_tail.as_ref());
                error!(
                    error = %error,
                    duration_ms,
                    exec_output_tail_present = tail.present,
                    exec_stdout_tail_bytes = tail.stdout_bytes,
                    exec_stderr_tail_bytes = tail.stderr_bytes,
                    exec_stdout_truncated = tail.stdout_truncated,
                    exec_stderr_truncated = tail.stderr_truncated,
                    "Retro failed"
                );
            }
        }
    }
}
