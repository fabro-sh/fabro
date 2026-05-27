use std::collections::BTreeMap;

use ::fabro_types::{
    AutomationRef, BilledTokenCounts, BlockedReason, CommandTermination, DiffSummary,
    FailureReason, ForkSourceRef, GitContext, PairId, PairMessageId, PairSystemMessageKind,
    PairTarget, ParallelBranchId, PendingReason, PermissionLevel, Principal, PullRequestLink,
    RunBlobId, RunFailure, RunId, RunNoticeLevel, RunPairEndedReason, RunPairFailedReason,
    RunProvenance, RunRunnableSource, RunTiming, SandboxProviderKind, StageId, StageTiming,
    SuccessReason, run_event as fabro_types,
};
use fabro_agent::{AgentEvent, SandboxEvent};
use fabro_model::{ReasoningEffort, Speed};
use serde::{Deserialize, Serialize};

use crate::error::{Error, run_failure_from_error};
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
        title:            Option<String>,
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
        automation:       Option<AutomationRef>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retried_from:     Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id:        Option<RunId>,
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
    RunStartRequested {
        resume: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:  Option<Principal>,
    },
    RunPending {
        reason: PendingReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:  Option<Principal>,
    },
    RunApproved {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunDenied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:  Option<Principal>,
    },
    RunRunnable {
        source: RunRunnableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:  Option<Principal>,
    },
    RunStarting,
    RunRunning,
    RunInterrupt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunSteer {
        text:  String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunPairStarted {
        pair_id: PairId,
        target:  PairTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:   Option<Principal>,
    },
    RunPairEnded {
        pair_id: PairId,
        reason:  RunPairEndedReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:   Option<Principal>,
    },
    RunPairFailed {
        pair_id: PairId,
        reason:  RunPairFailedReason,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:   Option<Principal>,
    },
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
    RunTitleUpdated {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<Principal>,
    },
    RunParentLinked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_parent_id: Option<RunId>,
        parent_id:          RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:              Option<Principal>,
    },
    RunParentUnlinked {
        previous_parent_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:              Option<Principal>,
    },
    WorkflowRunCompleted {
        timing:               RunTiming,
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
        diff_summary:         Option<DiffSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing:              Option<BilledTokenCounts>,
    },
    WorkflowRunFailed {
        failure:              RunFailure,
        timing:               RunTiming,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_git_commit_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_patch:          Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_summary:         Option<DiffSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing:              Option<BilledTokenCounts>,
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
        timing: StageTiming,
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
        timing:     StageTiming,
        billing:    Option<BilledModelUsage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:      Option<Principal>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_summary: Option<DiffSummary>,
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
        stage:            String,
        visit:            u32,
        text:             String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode:             Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider:         Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model:            Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speed:            Option<Speed>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id:      Option<String>,
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
        provider:          SandboxProviderKind,
        id:                String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image:             Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot:          Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo_cloned:       Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clone_origin_url:  Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clone_branch:      Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_root:    Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repos_root:        Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        primary_repo_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        primary_repo_link: Option<String>,
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
    CommandStarted {
        node_id:    String,
        script:     String,
        command:    String,
        language:   String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    CommandCompleted {
        node_id:        String,
        output:         String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code:      Option<i32>,
        duration_ms:    u64,
        termination:    CommandTermination,
        output_bytes:   u64,
        live_streaming: bool,
    },
    /// A top-level agent session object started its lifecycle.
    AgentSessionStarted {
        session_id:        String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider:          Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model:             Option<String>,
    },
    /// A stage has a currently steerable live session binding.
    AgentSessionActivated {
        node_id:          String,
        visit:            u32,
        session_id:       String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id:        Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider:         Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model:            Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speed:            Option<Speed>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_level: Option<PermissionLevel>,
        capabilities:     Vec<fabro_types::SessionCapability>,
    },
    /// Effective model-callable tools for a stage session after profile setup,
    /// optional registrations, MCP integration, and access-policy filtering.
    AgentToolsAvailable {
        node_id:    String,
        visit:      u32,
        session_id: String,
        tools:      Vec<fabro_types::AgentToolSummary>,
    },
    /// A stage's steerable live session binding ended.
    AgentSessionDeactivated {
        node_id:    String,
        visit:      u32,
        session_id: String,
    },
    /// A top-level agent session object ended its lifecycle.
    AgentSessionEnded {
        session_id:        String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
    },
    /// A run-level interrupt was delivered to a concrete steerable agent
    /// session/stage.
    AgentInterruptInjected {
        node_id:    String,
        visit:      u32,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:      Option<Principal>,
    },
    AgentPairUserMessage {
        node_id:           String,
        visit:             u32,
        session_id:        String,
        pair_id:           PairId,
        message_id:        PairMessageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_message_id: Option<String>,
        text:              String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor:             Option<Principal>,
    },
    AgentPairSystemMessage {
        node_id:    String,
        visit:      u32,
        session_id: String,
        pair_id:    PairId,
        kind:       PairSystemMessageKind,
        text:       String,
    },
    /// A steer arrived with no active session and was parked in the run-wide
    /// pending buffer. The actor (steer author) is lifted to top-level.
    AgentSteerBuffered {
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
    AgentAcpStarted {
        node_id:     String,
        visit:       u32,
        command:     String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_name: Option<String>,
    },
    AgentAcpCompleted {
        node_id:     String,
        stdout:      String,
        stderr:      String,
        stop_reason: String,
        duration_ms: u64,
    },
    AgentAcpCancelled {
        node_id:     String,
        stdout:      String,
        stderr:      String,
        duration_ms: u64,
    },
    AgentAcpTimedOut {
        node_id:     String,
        stdout:      String,
        stderr:      String,
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
    PullRequestLinked {
        pull_request: PullRequestLink,
    },
    PullRequestUnlinked {
        pull_request: PullRequestLink,
    },
    PullRequestFailed {
        error: String,
    },
}

impl Event {
    #[must_use]
    pub fn workflow_run_failed_from_error(
        error: &Error,
        timing: RunTiming,
        reason: FailureReason,
        final_git_commit_sha: Option<String>,
        final_patch: Option<String>,
        diff_summary: Option<DiffSummary>,
        billing: Option<BilledTokenCounts>,
    ) -> Self {
        Self::WorkflowRunFailed {
            failure: run_failure_from_error(error, reason),
            timing,
            final_git_commit_sha,
            final_patch,
            diff_summary,
            billing,
        }
    }

    pub fn pull_request_created(
        record: &PullRequestLink,
        base_branch: &str,
        head_branch: &str,
        title: &str,
        draft: bool,
    ) -> Self {
        Self::PullRequestCreated {
            pr_url: record.html_url(),
            pr_number: record.number,
            owner: record.owner.clone(),
            repo: record.repo.clone(),
            base_branch: base_branch.to_string(),
            head_branch: head_branch.to_string(),
            title: title.to_string(),
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
            Self::RunStartRequested { resume, .. } => {
                info!(resume, "Run start requested");
            }
            Self::RunPending { reason, .. } => {
                info!(?reason, "Run pending");
            }
            Self::RunApproved { .. } => {
                info!("Run approved");
            }
            Self::RunDenied { reason, .. } => {
                info!(?reason, "Run denied");
            }
            Self::RunRunnable { source, .. } => {
                info!(?source, "Run runnable");
            }
            Self::RunStarting => {
                info!("Run starting");
            }
            Self::RunRunning => {
                info!("Run running");
            }
            Self::RunInterrupt { .. } => {
                info!("Run interrupt accepted");
            }
            Self::RunSteer { text, .. } => {
                info!(text_len = text.len(), "Run steer accepted");
            }
            Self::RunPairStarted {
                pair_id, target, ..
            } => {
                info!(
                    %pair_id,
                    stage_id = %target.stage_id,
                    node_label = %target.node_label,
                    "Run pairing started",
                );
            }
            Self::RunPairEnded {
                pair_id, reason, ..
            } => {
                info!(%pair_id, ?reason, "Run pairing ended");
            }
            Self::RunPairFailed {
                pair_id,
                reason,
                message,
                ..
            } => {
                warn!(%pair_id, ?reason, message, "Run pairing failed");
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
            Self::RunTitleUpdated { title, actor } => {
                info!(title, ?actor, "Run title updated");
            }
            Self::RunParentLinked {
                previous_parent_id,
                parent_id,
                actor,
            } => {
                info!(?previous_parent_id, %parent_id, ?actor, "Run parent linked");
            }
            Self::RunParentUnlinked {
                previous_parent_id,
                actor,
            } => {
                info!(%previous_parent_id, ?actor, "Run parent unlinked");
            }
            Self::WorkflowRunCompleted {
                timing,
                artifact_count,
                status,
                ..
            } => {
                info!(
                    wall_time_ms = timing.wall_time_ms,
                    active_time_ms = timing.active_time_ms,
                    inference_time_ms = timing.inference_time_ms,
                    tool_time_ms = timing.tool_time_ms,
                    artifact_count,
                    status,
                    "Workflow run completed"
                );
            }
            Self::WorkflowRunFailed {
                failure, timing, ..
            } => {
                let detail = &failure.detail;
                let tail =
                    fabro_types::ExecOutputTail::trace_summary(detail.exec_output_tail.as_ref());
                error!(
                    message = %detail.message,
                    reason = %failure.reason,
                    category = %detail.category,
                    system_actor = ?detail.system_actor,
                    signature = ?detail.signature,
                    cause_count = detail.causes.len(),
                    exec_output_tail_present = tail.present,
                    exec_stdout_tail_bytes = tail.stdout_bytes,
                    exec_stderr_tail_bytes = tail.stderr_bytes,
                    exec_stdout_truncated = tail.stdout_truncated,
                    exec_stderr_truncated = tail.stderr_truncated,
                    wall_time_ms = timing.wall_time_ms,
                    active_time_ms = timing.active_time_ms,
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
                timing,
                status,
                attempt,
                max_attempts,
                ..
            } => {
                info!(
                    node_id,
                    stage = name.as_str(),
                    index,
                    wall_time_ms = timing.wall_time_ms,
                    active_time_ms = timing.active_time_ms,
                    inference_time_ms = timing.inference_time_ms,
                    tool_time_ms = timing.tool_time_ms,
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
                id,
                ..
            } => {
                info!(
                    working_directory,
                    provider = %provider,
                    id,
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
                output_bytes,
                ..
            } => {
                debug!(
                    node_id,
                    exit_code,
                    duration_ms,
                    termination = %termination,
                    output_bytes,
                    "Command completed"
                );
            }
            Self::AgentSessionStarted {
                session_id,
                provider,
                model,
                ..
            } => {
                debug!(session_id, ?provider, ?model, "Agent session started");
            }
            Self::AgentSessionActivated {
                node_id,
                visit,
                session_id,
                ..
            } => {
                debug!(node_id, visit, session_id, "Agent session activated");
            }
            Self::AgentToolsAvailable {
                node_id,
                visit,
                session_id,
                tools,
            } => {
                debug!(
                    node_id,
                    visit,
                    session_id,
                    tool_count = tools.len(),
                    "Agent tools available"
                );
            }
            Self::AgentSessionDeactivated {
                node_id,
                visit,
                session_id,
            } => {
                debug!(node_id, visit, session_id, "Agent session deactivated");
            }
            Self::AgentSessionEnded { session_id, .. } => {
                debug!(session_id, "Agent session ended");
            }
            Self::AgentInterruptInjected {
                node_id,
                visit,
                session_id,
                ..
            } => {
                debug!(node_id, visit, session_id, "Agent interrupt injected");
            }
            Self::AgentPairUserMessage {
                node_id,
                visit,
                session_id,
                pair_id,
                text,
                ..
            } => {
                debug!(node_id, visit, session_id, %pair_id, text_len = text.len(), "Agent pair user message accepted");
            }
            Self::AgentPairSystemMessage {
                node_id,
                visit,
                session_id,
                pair_id,
                kind,
                ..
            } => {
                debug!(node_id, visit, session_id, %pair_id, ?kind, "Agent pair system message queued");
            }
            Self::AgentSteerBuffered { .. } => {
                debug!("Steer buffered (no active session)");
            }
            Self::AgentSteerDropped { reason, count, .. } => {
                warn!(?reason, count, "Steer dropped");
            }
            Self::AgentAcpStarted {
                node_id,
                command,
                config_name,
                ..
            } => {
                debug!(node_id, command, ?config_name, "Agent ACP started");
            }
            Self::AgentAcpCompleted {
                node_id,
                stop_reason,
                duration_ms,
                ..
            } => {
                debug!(node_id, stop_reason, duration_ms, "Agent ACP completed");
            }
            Self::AgentAcpCancelled {
                node_id,
                duration_ms,
                ..
            } => {
                debug!(node_id, duration_ms, "Agent ACP cancelled");
            }
            Self::AgentAcpTimedOut {
                node_id,
                duration_ms,
                ..
            } => {
                debug!(node_id, duration_ms, "Agent ACP timed out");
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
            Self::PullRequestLinked { pull_request } => {
                info!(
                    pr_url = %pull_request.html_url(),
                    pr_number = pull_request.number,
                    "Pull request linked"
                );
            }
            Self::PullRequestUnlinked { pull_request } => {
                info!(
                    pr_url = %pull_request.html_url(),
                    pr_number = pull_request.number,
                    "Pull request unlinked"
                );
            }
            Self::PullRequestFailed { error, .. } => {
                error!(error = %error, "Pull request creation failed");
            }
        }
    }
}
