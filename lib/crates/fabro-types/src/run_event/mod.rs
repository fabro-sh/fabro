pub mod agent;
pub mod infra;
pub mod misc;
pub mod run;
pub mod session;
pub mod stage;
pub mod todo;

pub use agent::*;
use chrono::{DateTime, Utc};
pub use fabro_model::BilledTokenCounts;
pub use infra::*;
pub use misc::*;
pub use run::*;
use serde::de::Error as DeError;
use serde::ser::Error as SerError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
pub use session::*;
pub use stage::*;
pub use todo::*;

use crate::{ParallelBranchId, Principal, RunId, StageId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunNoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunEvent {
    pub id:                 String,
    pub ts:                 DateTime<Utc>,
    pub run_id:             RunId,
    pub node_id:            Option<String>,
    pub node_label:         Option<String>,
    pub stage_id:           Option<StageId>,
    pub parallel_group_id:  Option<StageId>,
    pub parallel_branch_id: Option<ParallelBranchId>,
    pub session_id:         Option<String>,
    pub parent_session_id:  Option<String>,
    pub tool_call_id:       Option<String>,
    pub actor:              Option<Principal>,
    pub body:               EventBody,
}

#[allow(
    clippy::large_enum_variant,
    reason = "Run event bodies stay inline to match the tagged wire format."
)]
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "event", content = "properties")]
pub enum EventBody {
    #[serde(rename = "run.created")]
    RunCreated(RunCreatedProps),
    #[serde(rename = "run.started")]
    RunStarted(RunStartedProps),
    #[serde(rename = "run.submitted")]
    RunSubmitted(RunSubmittedProps),
    #[serde(rename = "run.start_requested")]
    RunStartRequested(RunStartRequestedProps),
    #[serde(rename = "run.pending")]
    RunPending(RunPendingProps),
    #[serde(rename = "run.approved")]
    RunApproved(RunApprovedProps),
    #[serde(rename = "run.denied")]
    RunDenied(RunDeniedProps),
    #[serde(rename = "run.runnable")]
    RunRunnable(RunRunnableProps),
    #[serde(rename = "run.starting")]
    RunStarting(RunStatusTransitionProps),
    #[serde(rename = "run.running")]
    RunRunning(RunStatusTransitionProps),
    #[serde(rename = "run.interrupt")]
    RunInterrupt(RunInterruptProps),
    #[serde(rename = "run.steer")]
    RunSteer(RunSteerProps),
    #[serde(rename = "run.pair.started")]
    RunPairStarted(RunPairStartedProps),
    #[serde(rename = "run.pair.ended")]
    RunPairEnded(RunPairEndedProps),
    #[serde(rename = "run.pair.failed")]
    RunPairFailed(RunPairFailedProps),
    #[serde(rename = "run.blocked")]
    RunBlocked(RunBlockedProps),
    #[serde(rename = "run.unblocked")]
    RunUnblocked(RunStatusEffectProps),
    #[serde(rename = "run.removing")]
    RunRemoving(RunStatusTransitionProps),
    #[serde(rename = "run.cancel.requested")]
    RunCancelRequested(RunControlRequestedProps),
    #[serde(rename = "run.pause.requested")]
    RunPauseRequested(RunControlRequestedProps),
    #[serde(rename = "run.unpause.requested")]
    RunUnpauseRequested(RunControlRequestedProps),
    #[serde(rename = "run.paused")]
    RunPaused(RunControlEffectProps),
    #[serde(rename = "run.unpaused")]
    RunUnpaused(RunControlEffectProps),
    #[serde(rename = "run.superseded_by")]
    RunSupersededBy(RunSupersededByProps),
    #[serde(rename = "run.archived")]
    RunArchived(RunArchivedProps),
    #[serde(rename = "run.unarchived")]
    RunUnarchived(RunUnarchivedProps),
    #[serde(rename = "run.title.updated")]
    RunTitleUpdated(RunTitleUpdatedProps),
    #[serde(rename = "run.session.created")]
    RunSessionCreated(RunSessionCreatedProps),
    #[serde(rename = "run.session.turn.started")]
    RunSessionTurnStarted(RunSessionTurnStartedProps),
    #[serde(rename = "run.session.user_message")]
    RunSessionUserMessage(RunSessionUserMessageProps),
    #[serde(rename = "run.session.assistant_delta")]
    RunSessionAssistantDelta(RunSessionAssistantDeltaProps),
    #[serde(rename = "run.session.assistant_message")]
    RunSessionAssistantMessage(RunSessionAssistantMessageProps),
    #[serde(rename = "run.session.tool_call.started")]
    RunSessionToolCallStarted(RunSessionToolCallStartedProps),
    #[serde(rename = "run.session.tool_call.completed")]
    RunSessionToolCallCompleted(RunSessionToolCallCompletedProps),
    #[serde(rename = "run.session.turn.succeeded")]
    RunSessionTurnSucceeded(RunSessionTurnSucceededProps),
    #[serde(rename = "run.session.turn.failed")]
    RunSessionTurnFailed(RunSessionTurnFailedProps),
    #[serde(rename = "run.session.turn.interrupted")]
    RunSessionTurnInterrupted(RunSessionTurnInterruptedProps),
    #[serde(rename = "run.parent.linked")]
    RunParentLinked(RunParentLinkedProps),
    #[serde(rename = "run.parent.unlinked")]
    RunParentUnlinked(RunParentUnlinkedProps),
    #[serde(rename = "run.completed")]
    RunCompleted(RunCompletedProps),
    #[serde(rename = "run.failed")]
    RunFailed(RunFailedProps),
    #[serde(rename = "run.notice")]
    RunNotice(RunNoticeProps),
    #[serde(rename = "metadata.snapshot.started")]
    MetadataSnapshotStarted(MetadataSnapshotStartedProps),
    #[serde(rename = "metadata.snapshot.completed")]
    MetadataSnapshotCompleted(MetadataSnapshotCompletedProps),
    #[serde(rename = "metadata.snapshot.failed")]
    MetadataSnapshotFailed(MetadataSnapshotFailedProps),
    #[serde(rename = "stage.started")]
    StageStarted(StageStartedProps),
    #[serde(rename = "stage.completed")]
    StageCompleted(StageCompletedProps),
    #[serde(rename = "stage.failed")]
    StageFailed(StageFailedProps),
    #[serde(rename = "stage.retrying")]
    StageRetrying(StageRetryingProps),
    #[serde(rename = "parallel.started")]
    ParallelStarted(ParallelStartedProps),
    #[serde(rename = "parallel.branch.started")]
    ParallelBranchStarted(ParallelBranchStartedProps),
    #[serde(rename = "parallel.branch.completed")]
    ParallelBranchCompleted(ParallelBranchCompletedProps),
    #[serde(rename = "parallel.completed")]
    ParallelCompleted(ParallelCompletedProps),
    #[serde(rename = "interview.started")]
    InterviewStarted(InterviewStartedProps),
    #[serde(rename = "interview.completed")]
    InterviewCompleted(InterviewCompletedProps),
    #[serde(rename = "interview.timeout")]
    InterviewTimeout(InterviewTimeoutProps),
    #[serde(rename = "interview.interrupted")]
    InterviewInterrupted(InterviewInterruptedProps),
    #[serde(rename = "checkpoint.completed")]
    CheckpointCompleted(CheckpointCompletedProps),
    #[serde(rename = "checkpoint.failed")]
    CheckpointFailed(CheckpointFailedProps),
    #[serde(rename = "git.commit")]
    GitCommit(GitCommitProps),
    #[serde(rename = "git.push")]
    GitPush(GitPushProps),
    #[serde(rename = "git.branch")]
    GitBranch(GitBranchProps),
    #[serde(rename = "git.worktree.added")]
    GitWorktreeAdd(GitWorktreeAddProps),
    #[serde(rename = "git.worktree.removed")]
    GitWorktreeRemove(GitWorktreeRemoveProps),
    #[serde(rename = "git.fetch")]
    GitFetch(GitFetchProps),
    #[serde(rename = "git.reset")]
    GitReset(GitResetProps),
    #[serde(rename = "edge.selected")]
    EdgeSelected(EdgeSelectedProps),
    #[serde(rename = "loop.restart")]
    LoopRestart(LoopRestartProps),
    #[serde(rename = "stage.prompt")]
    StagePrompt(StagePromptProps),
    #[serde(rename = "prompt.completed")]
    PromptCompleted(PromptCompletedProps),
    #[serde(rename = "agent.session.started")]
    AgentSessionStarted(AgentSessionStartedProps),
    #[serde(rename = "agent.session.activated")]
    AgentSessionActivated(AgentSessionActivatedProps),
    #[serde(rename = "agent.session.deactivated")]
    AgentSessionDeactivated(AgentSessionDeactivatedProps),
    #[serde(rename = "agent.session.ended")]
    AgentSessionEnded(AgentSessionEndedProps),
    #[serde(rename = "agent.processing.end")]
    AgentProcessingEnd(AgentProcessingEndProps),
    #[serde(rename = "agent.input")]
    AgentInput(AgentInputProps),
    #[serde(rename = "agent.message")]
    AgentMessage(AgentMessageProps),
    #[serde(rename = "agent.tool.started")]
    AgentToolStarted(AgentToolStartedProps),
    #[serde(rename = "agent.tool.completed")]
    AgentToolCompleted(AgentToolCompletedProps),
    #[serde(rename = "agent.error")]
    AgentError(AgentErrorProps),
    #[serde(rename = "agent.warning")]
    AgentWarning(AgentWarningProps),
    #[serde(rename = "agent.loop.detected")]
    AgentLoopDetected(AgentLoopDetectedProps),
    #[serde(rename = "agent.turn.limit")]
    AgentTurnLimitReached(AgentTurnLimitReachedProps),
    #[serde(rename = "agent.steering.injected")]
    AgentSteeringInjected(AgentSteeringInjectedProps),
    #[serde(rename = "agent.pair.user_message")]
    AgentPairUserMessage(AgentPairUserMessageProps),
    #[serde(rename = "agent.pair.system_message")]
    AgentPairSystemMessage(AgentPairSystemMessageProps),
    #[serde(rename = "agent.interrupt.injected")]
    AgentInterruptInjected(AgentInterruptInjectedProps),
    #[serde(rename = "agent.steer.buffered")]
    AgentSteerBuffered(AgentSteerBufferedProps),
    #[serde(rename = "agent.steer.dropped")]
    AgentSteerDropped(AgentSteerDroppedProps),
    #[serde(rename = "agent.compaction.started")]
    AgentCompactionStarted(AgentCompactionStartedProps),
    #[serde(rename = "agent.compaction.completed")]
    AgentCompactionCompleted(AgentCompactionCompletedProps),
    #[serde(rename = "agent.llm.retry")]
    AgentLlmRetry(AgentLlmRetryProps),
    #[serde(rename = "agent.context_window.snapshot")]
    AgentContextWindowSnapshot(AgentContextWindowSnapshotProps),
    #[serde(rename = "agent.sub.spawned")]
    AgentSubSpawned(AgentSubSpawnedProps),
    #[serde(rename = "agent.sub.completed")]
    AgentSubCompleted(AgentSubCompletedProps),
    #[serde(rename = "agent.sub.failed")]
    AgentSubFailed(AgentSubFailedProps),
    #[serde(rename = "agent.sub.closed")]
    AgentSubClosed(AgentSubClosedProps),
    #[serde(rename = "agent.mcp.ready")]
    AgentMcpReady(AgentMcpReadyProps),
    #[serde(rename = "agent.mcp.failed")]
    AgentMcpFailed(AgentMcpFailedProps),
    #[serde(rename = "agent.memory.loaded")]
    AgentMemoryLoaded(AgentMemoryLoadedProps),
    #[serde(rename = "agent.skills.discovered")]
    AgentSkillsDiscovered(AgentSkillsDiscoveredProps),
    #[serde(rename = "agent.skill.activated")]
    AgentSkillActivated(AgentSkillActivatedProps),
    #[serde(rename = "todo.created")]
    TodoCreated(TodoCreatedProps),
    #[serde(rename = "todo.updated")]
    TodoUpdated(TodoUpdatedProps),
    #[serde(rename = "todo.deleted")]
    TodoDeleted(TodoDeletedProps),
    #[serde(rename = "subgraph.started")]
    SubgraphStarted(SubgraphStartedProps),
    #[serde(rename = "subgraph.completed")]
    SubgraphCompleted(SubgraphCompletedProps),
    #[serde(rename = "sandbox.initializing")]
    SandboxInitializing(SandboxInitializingProps),
    #[serde(rename = "sandbox.ready")]
    SandboxReady(SandboxReadyProps),
    #[serde(rename = "sandbox.failed")]
    SandboxFailed(SandboxFailedProps),
    #[serde(rename = "sandbox.cleanup.started")]
    SandboxCleanupStarted(SandboxCleanupStartedProps),
    #[serde(rename = "sandbox.cleanup.completed")]
    SandboxCleanupCompleted(SandboxCleanupCompletedProps),
    #[serde(rename = "sandbox.cleanup.failed")]
    SandboxCleanupFailed(SandboxCleanupFailedProps),
    #[serde(rename = "sandbox.start.started")]
    SandboxStartStarted(SandboxStartStartedProps),
    #[serde(rename = "sandbox.start.completed")]
    SandboxStartCompleted(SandboxStartCompletedProps),
    #[serde(rename = "sandbox.start.failed")]
    SandboxStartFailed(SandboxStartFailedProps),
    #[serde(rename = "sandbox.stop.started")]
    SandboxStopStarted(SandboxStopStartedProps),
    #[serde(rename = "sandbox.stop.completed")]
    SandboxStopCompleted(SandboxStopCompletedProps),
    #[serde(rename = "sandbox.stop.failed")]
    SandboxStopFailed(SandboxStopFailedProps),
    #[serde(rename = "sandbox.delete.started")]
    SandboxDeleteStarted(SandboxDeleteStartedProps),
    #[serde(rename = "sandbox.delete.completed")]
    SandboxDeleteCompleted(SandboxDeleteCompletedProps),
    #[serde(rename = "sandbox.delete.failed")]
    SandboxDeleteFailed(SandboxDeleteFailedProps),
    #[serde(rename = "sandbox.snapshot.pulling")]
    SnapshotPulling(SnapshotNameProps),
    #[serde(rename = "sandbox.snapshot.creating")]
    SnapshotCreating(SnapshotNameProps),
    #[serde(rename = "sandbox.snapshot.ready")]
    SnapshotReady(SnapshotCompletedProps),
    #[serde(rename = "sandbox.snapshot.failed")]
    SnapshotFailed(SnapshotFailedProps),
    #[serde(rename = "sandbox.git.started")]
    GitCloneStarted(GitCloneStartedProps),
    #[serde(rename = "sandbox.git.completed")]
    GitCloneCompleted(GitCloneCompletedProps),
    #[serde(rename = "sandbox.git.failed")]
    GitCloneFailed(GitCloneFailedProps),
    #[serde(rename = "sandbox.initialized")]
    SandboxInitialized(SandboxInitializedProps),
    #[serde(rename = "setup.started")]
    SetupStarted(SetupStartedProps),
    #[serde(rename = "setup.command.started")]
    SetupCommandStarted(SetupCommandStartedProps),
    #[serde(rename = "setup.command.completed")]
    SetupCommandCompleted(SetupCommandCompletedProps),
    #[serde(rename = "setup.completed")]
    SetupCompleted(SetupCompletedProps),
    #[serde(rename = "setup.failed")]
    SetupFailed(SetupFailedProps),
    #[serde(rename = "watchdog.timeout")]
    StallWatchdogTimeout(StallWatchdogTimeoutProps),
    #[serde(rename = "artifact.captured")]
    ArtifactCaptured(ArtifactCapturedProps),
    #[serde(rename = "ssh.ready")]
    SshAccessReady(SshAccessReadyProps),
    #[serde(rename = "agent.failover")]
    Failover(FailoverProps),
    #[serde(rename = "cli.ensure.started")]
    CliEnsureStarted(CliEnsureStartedProps),
    #[serde(rename = "cli.ensure.completed")]
    CliEnsureCompleted(CliEnsureCompletedProps),
    #[serde(rename = "cli.ensure.failed")]
    CliEnsureFailed(CliEnsureFailedProps),
    #[serde(rename = "command.started")]
    CommandStarted(CommandStartedProps),
    #[serde(rename = "command.completed")]
    CommandCompleted(CommandCompletedProps),
    #[serde(rename = "agent.acp.started")]
    AgentAcpStarted(AgentAcpStartedProps),
    #[serde(rename = "agent.acp.completed")]
    AgentAcpCompleted(AgentAcpCompletedProps),
    #[serde(rename = "agent.acp.cancelled")]
    AgentAcpCancelled(AgentAcpCancelledProps),
    #[serde(rename = "agent.acp.timed_out")]
    AgentAcpTimedOut(AgentAcpTimedOutProps),
    #[serde(rename = "pull_request.created")]
    PullRequestCreated(PullRequestCreatedProps),
    #[serde(rename = "pull_request.linked")]
    PullRequestLinked(PullRequestLinkedProps),
    #[serde(rename = "pull_request.unlinked")]
    PullRequestUnlinked(PullRequestUnlinkedProps),
    #[serde(rename = "pull_request.failed")]
    PullRequestFailed(PullRequestFailedProps),
    #[serde(rename = "devcontainer.resolved")]
    DevcontainerResolved(DevcontainerResolvedProps),
    #[serde(rename = "devcontainer.lifecycle.started")]
    DevcontainerLifecycleStarted(DevcontainerLifecycleStartedProps),
    #[serde(rename = "devcontainer.lifecycle.command.started")]
    DevcontainerLifecycleCommandStarted(DevcontainerLifecycleCommandStartedProps),
    #[serde(rename = "devcontainer.lifecycle.command.completed")]
    DevcontainerLifecycleCommandCompleted(DevcontainerLifecycleCommandCompletedProps),
    #[serde(rename = "devcontainer.lifecycle.completed")]
    DevcontainerLifecycleCompleted(DevcontainerLifecycleCompletedProps),
    #[serde(rename = "devcontainer.lifecycle.failed")]
    DevcontainerLifecycleFailed(DevcontainerLifecycleFailedProps),
    Unknown {
        name:       String,
        properties: Value,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct RunEventRaw {
    id:                 String,
    ts:                 DateTime<Utc>,
    run_id:             RunId,
    #[serde(default)]
    node_id:            Option<String>,
    #[serde(default)]
    node_label:         Option<String>,
    #[serde(default)]
    stage_id:           Option<StageId>,
    #[serde(default)]
    parallel_group_id:  Option<StageId>,
    #[serde(default)]
    parallel_branch_id: Option<ParallelBranchId>,
    #[serde(default)]
    session_id:         Option<String>,
    #[serde(default)]
    parent_session_id:  Option<String>,
    #[serde(default)]
    tool_call_id:       Option<String>,
    #[serde(default)]
    actor:              Option<Principal>,
    event:              String,
    #[serde(default = "default_properties")]
    properties:         Value,
}

fn default_properties() -> Value {
    Value::Object(Map::new())
}

struct RunEventParts<'a> {
    id:                 String,
    ts:                 DateTime<Utc>,
    run_id:             RunId,
    node_id:            Option<String>,
    node_label:         Option<String>,
    stage_id:           Option<StageId>,
    parallel_group_id:  Option<StageId>,
    parallel_branch_id: Option<ParallelBranchId>,
    session_id:         Option<String>,
    parent_session_id:  Option<String>,
    tool_call_id:       Option<String>,
    actor:              Option<Principal>,
    event:              &'a str,
    properties:         &'a Value,
}

impl EventBody {
    pub fn event_name(&self) -> &str {
        match self {
            Self::RunCreated(_) => "run.created",
            Self::RunStarted(_) => "run.started",
            Self::RunSubmitted(_) => "run.submitted",
            Self::RunStartRequested(_) => "run.start_requested",
            Self::RunPending(_) => "run.pending",
            Self::RunApproved(_) => "run.approved",
            Self::RunDenied(_) => "run.denied",
            Self::RunRunnable(_) => "run.runnable",
            Self::RunStarting(_) => "run.starting",
            Self::RunRunning(_) => "run.running",
            Self::RunInterrupt(_) => "run.interrupt",
            Self::RunSteer(_) => "run.steer",
            Self::RunPairStarted(_) => "run.pair.started",
            Self::RunPairEnded(_) => "run.pair.ended",
            Self::RunPairFailed(_) => "run.pair.failed",
            Self::RunBlocked(_) => "run.blocked",
            Self::RunUnblocked(_) => "run.unblocked",
            Self::RunRemoving(_) => "run.removing",
            Self::RunCancelRequested(_) => "run.cancel.requested",
            Self::RunPauseRequested(_) => "run.pause.requested",
            Self::RunUnpauseRequested(_) => "run.unpause.requested",
            Self::RunPaused(_) => "run.paused",
            Self::RunUnpaused(_) => "run.unpaused",
            Self::RunSupersededBy(_) => "run.superseded_by",
            Self::RunArchived(_) => "run.archived",
            Self::RunUnarchived(_) => "run.unarchived",
            Self::RunTitleUpdated(_) => "run.title.updated",
            Self::RunSessionCreated(_) => "run.session.created",
            Self::RunSessionTurnStarted(_) => "run.session.turn.started",
            Self::RunSessionUserMessage(_) => "run.session.user_message",
            Self::RunSessionAssistantDelta(_) => "run.session.assistant_delta",
            Self::RunSessionAssistantMessage(_) => "run.session.assistant_message",
            Self::RunSessionToolCallStarted(_) => "run.session.tool_call.started",
            Self::RunSessionToolCallCompleted(_) => "run.session.tool_call.completed",
            Self::RunSessionTurnSucceeded(_) => "run.session.turn.succeeded",
            Self::RunSessionTurnFailed(_) => "run.session.turn.failed",
            Self::RunSessionTurnInterrupted(_) => "run.session.turn.interrupted",
            Self::RunParentLinked(_) => "run.parent.linked",
            Self::RunParentUnlinked(_) => "run.parent.unlinked",
            Self::RunCompleted(_) => "run.completed",
            Self::RunFailed(_) => "run.failed",
            Self::RunNotice(_) => "run.notice",
            Self::MetadataSnapshotStarted(_) => "metadata.snapshot.started",
            Self::MetadataSnapshotCompleted(_) => "metadata.snapshot.completed",
            Self::MetadataSnapshotFailed(_) => "metadata.snapshot.failed",
            Self::StageStarted(_) => "stage.started",
            Self::StageCompleted(_) => "stage.completed",
            Self::StageFailed(_) => "stage.failed",
            Self::StageRetrying(_) => "stage.retrying",
            Self::ParallelStarted(_) => "parallel.started",
            Self::ParallelBranchStarted(_) => "parallel.branch.started",
            Self::ParallelBranchCompleted(_) => "parallel.branch.completed",
            Self::ParallelCompleted(_) => "parallel.completed",
            Self::InterviewStarted(_) => "interview.started",
            Self::InterviewCompleted(_) => "interview.completed",
            Self::InterviewTimeout(_) => "interview.timeout",
            Self::InterviewInterrupted(_) => "interview.interrupted",
            Self::CheckpointCompleted(_) => "checkpoint.completed",
            Self::CheckpointFailed(_) => "checkpoint.failed",
            Self::GitCommit(_) => "git.commit",
            Self::GitPush(_) => "git.push",
            Self::GitBranch(_) => "git.branch",
            Self::GitWorktreeAdd(_) => "git.worktree.added",
            Self::GitWorktreeRemove(_) => "git.worktree.removed",
            Self::GitFetch(_) => "git.fetch",
            Self::GitReset(_) => "git.reset",
            Self::EdgeSelected(_) => "edge.selected",
            Self::LoopRestart(_) => "loop.restart",
            Self::StagePrompt(_) => "stage.prompt",
            Self::PromptCompleted(_) => "prompt.completed",
            Self::AgentSessionStarted(_) => "agent.session.started",
            Self::AgentSessionActivated(_) => "agent.session.activated",
            Self::AgentSessionDeactivated(_) => "agent.session.deactivated",
            Self::AgentSessionEnded(_) => "agent.session.ended",
            Self::AgentProcessingEnd(_) => "agent.processing.end",
            Self::AgentInput(_) => "agent.input",
            Self::AgentMessage(_) => "agent.message",
            Self::AgentToolStarted(_) => "agent.tool.started",
            Self::AgentToolCompleted(_) => "agent.tool.completed",
            Self::AgentError(_) => "agent.error",
            Self::AgentWarning(_) => "agent.warning",
            Self::AgentLoopDetected(_) => "agent.loop.detected",
            Self::AgentTurnLimitReached(_) => "agent.turn.limit",
            Self::AgentSteeringInjected(_) => "agent.steering.injected",
            Self::AgentPairUserMessage(_) => "agent.pair.user_message",
            Self::AgentPairSystemMessage(_) => "agent.pair.system_message",
            Self::AgentInterruptInjected(_) => "agent.interrupt.injected",
            Self::AgentSteerBuffered(_) => "agent.steer.buffered",
            Self::AgentSteerDropped(_) => "agent.steer.dropped",
            Self::AgentCompactionStarted(_) => "agent.compaction.started",
            Self::AgentCompactionCompleted(_) => "agent.compaction.completed",
            Self::AgentLlmRetry(_) => "agent.llm.retry",
            Self::AgentContextWindowSnapshot(_) => "agent.context_window.snapshot",
            Self::AgentSubSpawned(_) => "agent.sub.spawned",
            Self::AgentSubCompleted(_) => "agent.sub.completed",
            Self::AgentSubFailed(_) => "agent.sub.failed",
            Self::AgentSubClosed(_) => "agent.sub.closed",
            Self::AgentMcpReady(_) => "agent.mcp.ready",
            Self::AgentMcpFailed(_) => "agent.mcp.failed",
            Self::AgentMemoryLoaded(_) => "agent.memory.loaded",
            Self::AgentSkillsDiscovered(_) => "agent.skills.discovered",
            Self::AgentSkillActivated(_) => "agent.skill.activated",
            Self::TodoCreated(_) => "todo.created",
            Self::TodoUpdated(_) => "todo.updated",
            Self::TodoDeleted(_) => "todo.deleted",
            Self::SubgraphStarted(_) => "subgraph.started",
            Self::SubgraphCompleted(_) => "subgraph.completed",
            Self::SandboxInitializing(_) => "sandbox.initializing",
            Self::SandboxReady(_) => "sandbox.ready",
            Self::SandboxFailed(_) => "sandbox.failed",
            Self::SandboxCleanupStarted(_) => "sandbox.cleanup.started",
            Self::SandboxCleanupCompleted(_) => "sandbox.cleanup.completed",
            Self::SandboxCleanupFailed(_) => "sandbox.cleanup.failed",
            Self::SandboxStartStarted(_) => "sandbox.start.started",
            Self::SandboxStartCompleted(_) => "sandbox.start.completed",
            Self::SandboxStartFailed(_) => "sandbox.start.failed",
            Self::SandboxStopStarted(_) => "sandbox.stop.started",
            Self::SandboxStopCompleted(_) => "sandbox.stop.completed",
            Self::SandboxStopFailed(_) => "sandbox.stop.failed",
            Self::SandboxDeleteStarted(_) => "sandbox.delete.started",
            Self::SandboxDeleteCompleted(_) => "sandbox.delete.completed",
            Self::SandboxDeleteFailed(_) => "sandbox.delete.failed",
            Self::SnapshotPulling(_) => "sandbox.snapshot.pulling",
            Self::SnapshotCreating(_) => "sandbox.snapshot.creating",
            Self::SnapshotReady(_) => "sandbox.snapshot.ready",
            Self::SnapshotFailed(_) => "sandbox.snapshot.failed",
            Self::GitCloneStarted(_) => "sandbox.git.started",
            Self::GitCloneCompleted(_) => "sandbox.git.completed",
            Self::GitCloneFailed(_) => "sandbox.git.failed",
            Self::SandboxInitialized(_) => "sandbox.initialized",
            Self::SetupStarted(_) => "setup.started",
            Self::SetupCommandStarted(_) => "setup.command.started",
            Self::SetupCommandCompleted(_) => "setup.command.completed",
            Self::SetupCompleted(_) => "setup.completed",
            Self::SetupFailed(_) => "setup.failed",
            Self::StallWatchdogTimeout(_) => "watchdog.timeout",
            Self::ArtifactCaptured(_) => "artifact.captured",
            Self::SshAccessReady(_) => "ssh.ready",
            Self::Failover(_) => "agent.failover",
            Self::CliEnsureStarted(_) => "cli.ensure.started",
            Self::CliEnsureCompleted(_) => "cli.ensure.completed",
            Self::CliEnsureFailed(_) => "cli.ensure.failed",
            Self::CommandStarted(_) => "command.started",
            Self::CommandCompleted(_) => "command.completed",
            Self::AgentAcpStarted(_) => "agent.acp.started",
            Self::AgentAcpCompleted(_) => "agent.acp.completed",
            Self::AgentAcpCancelled(_) => "agent.acp.cancelled",
            Self::AgentAcpTimedOut(_) => "agent.acp.timed_out",
            Self::PullRequestCreated(_) => "pull_request.created",
            Self::PullRequestLinked(_) => "pull_request.linked",
            Self::PullRequestUnlinked(_) => "pull_request.unlinked",
            Self::PullRequestFailed(_) => "pull_request.failed",
            Self::DevcontainerResolved(_) => "devcontainer.resolved",
            Self::DevcontainerLifecycleStarted(_) => "devcontainer.lifecycle.started",
            Self::DevcontainerLifecycleCommandStarted(_) => {
                "devcontainer.lifecycle.command.started"
            }
            Self::DevcontainerLifecycleCommandCompleted(_) => {
                "devcontainer.lifecycle.command.completed"
            }
            Self::DevcontainerLifecycleCompleted(_) => "devcontainer.lifecycle.completed",
            Self::DevcontainerLifecycleFailed(_) => "devcontainer.lifecycle.failed",
            Self::Unknown { name, .. } => name.as_str(),
        }
    }

    pub fn is_run_session_event(&self) -> bool {
        self.event_name().starts_with("run.session.")
    }

    fn properties_value(&self) -> serde_json::Result<Value> {
        if let Self::Unknown { properties, .. } = self {
            return Ok(properties.clone());
        }

        match serde_json::to_value(self)? {
            Value::Object(mut map) => {
                Ok(map.remove("properties").unwrap_or_else(default_properties))
            }
            _ => Ok(default_properties()),
        }
    }
}

fn is_known_event_name(event: &str) -> bool {
    matches!(
        event,
        "run.created"
            | "run.started"
            | "run.submitted"
            | "run.start_requested"
            | "run.pending"
            | "run.approved"
            | "run.denied"
            | "run.runnable"
            | "run.starting"
            | "run.running"
            | "run.interrupt"
            | "run.steer"
            | "run.pair.started"
            | "run.pair.ended"
            | "run.pair.failed"
            | "run.blocked"
            | "run.unblocked"
            | "run.removing"
            | "run.superseded_by"
            | "run.archived"
            | "run.unarchived"
            | "run.title.updated"
            | "run.session.created"
            | "run.session.turn.started"
            | "run.session.user_message"
            | "run.session.assistant_delta"
            | "run.session.assistant_message"
            | "run.session.tool_call.started"
            | "run.session.tool_call.completed"
            | "run.session.turn.succeeded"
            | "run.session.turn.failed"
            | "run.session.turn.interrupted"
            | "run.parent.linked"
            | "run.parent.unlinked"
            | "run.completed"
            | "run.failed"
            | "run.notice"
            | "metadata.snapshot.started"
            | "metadata.snapshot.completed"
            | "metadata.snapshot.failed"
            | "stage.started"
            | "stage.completed"
            | "stage.failed"
            | "stage.retrying"
            | "parallel.started"
            | "parallel.branch.started"
            | "parallel.branch.completed"
            | "parallel.completed"
            | "interview.started"
            | "interview.completed"
            | "interview.timeout"
            | "interview.interrupted"
            | "checkpoint.completed"
            | "checkpoint.failed"
            | "git.commit"
            | "git.push"
            | "git.branch"
            | "git.worktree.added"
            | "git.worktree.removed"
            | "git.fetch"
            | "git.reset"
            | "edge.selected"
            | "loop.restart"
            | "stage.prompt"
            | "prompt.completed"
            | "agent.session.started"
            | "agent.session.activated"
            | "agent.session.deactivated"
            | "agent.session.ended"
            | "agent.processing.end"
            | "agent.input"
            | "agent.message"
            | "agent.tool.started"
            | "agent.tool.completed"
            | "agent.error"
            | "agent.warning"
            | "agent.loop.detected"
            | "agent.turn.limit"
            | "agent.steering.injected"
            | "agent.pair.user_message"
            | "agent.pair.system_message"
            | "agent.interrupt.injected"
            | "agent.steer.buffered"
            | "agent.steer.dropped"
            | "agent.compaction.started"
            | "agent.compaction.completed"
            | "agent.llm.retry"
            | "agent.context_window.snapshot"
            | "agent.sub.spawned"
            | "agent.sub.completed"
            | "agent.sub.failed"
            | "agent.sub.closed"
            | "agent.mcp.ready"
            | "agent.mcp.failed"
            | "agent.memory.loaded"
            | "agent.skills.discovered"
            | "agent.skill.activated"
            | "todo.created"
            | "todo.updated"
            | "todo.deleted"
            | "subgraph.started"
            | "subgraph.completed"
            | "sandbox.initializing"
            | "sandbox.ready"
            | "sandbox.failed"
            | "sandbox.cleanup.started"
            | "sandbox.cleanup.completed"
            | "sandbox.cleanup.failed"
            | "sandbox.start.started"
            | "sandbox.start.completed"
            | "sandbox.start.failed"
            | "sandbox.stop.started"
            | "sandbox.stop.completed"
            | "sandbox.stop.failed"
            | "sandbox.delete.started"
            | "sandbox.delete.completed"
            | "sandbox.delete.failed"
            | "sandbox.snapshot.pulling"
            | "sandbox.snapshot.creating"
            | "sandbox.snapshot.ready"
            | "sandbox.snapshot.failed"
            | "sandbox.git.started"
            | "sandbox.git.completed"
            | "sandbox.git.failed"
            | "sandbox.initialized"
            | "setup.started"
            | "setup.command.started"
            | "setup.command.completed"
            | "setup.completed"
            | "setup.failed"
            | "watchdog.timeout"
            | "artifact.captured"
            | "ssh.ready"
            | "agent.failover"
            | "cli.ensure.started"
            | "cli.ensure.completed"
            | "cli.ensure.failed"
            | "command.started"
            | "command.completed"
            | "agent.acp.started"
            | "agent.acp.completed"
            | "agent.acp.cancelled"
            | "agent.acp.timed_out"
            | "pull_request.created"
            | "pull_request.linked"
            | "pull_request.unlinked"
            | "pull_request.failed"
            | "devcontainer.resolved"
            | "devcontainer.lifecycle.started"
            | "devcontainer.lifecycle.command.started"
            | "devcontainer.lifecycle.command.completed"
            | "devcontainer.lifecycle.completed"
            | "devcontainer.lifecycle.failed"
    )
}

impl RunEvent {
    pub fn from_value(value: Value) -> serde_json::Result<Self> {
        let raw: RunEventRaw = serde_json::from_value(value)?;
        Self::from_parts(RunEventParts {
            id:                 raw.id,
            ts:                 raw.ts,
            run_id:             raw.run_id,
            node_id:            raw.node_id,
            node_label:         raw.node_label,
            stage_id:           raw.stage_id,
            parallel_group_id:  raw.parallel_group_id,
            parallel_branch_id: raw.parallel_branch_id,
            session_id:         raw.session_id,
            parent_session_id:  raw.parent_session_id,
            tool_call_id:       raw.tool_call_id,
            actor:              raw.actor,
            event:              &raw.event,
            properties:         &raw.properties,
        })
    }

    pub fn from_ref(value: &Value) -> serde_json::Result<Self> {
        fn opt_field<T: for<'a> Deserialize<'a>>(
            obj: &Map<String, Value>,
            key: &str,
        ) -> serde_json::Result<Option<T>> {
            match obj.get(key) {
                Some(value) if !value.is_null() => Ok(Some(T::deserialize(value)?)),
                _ => Ok(None),
            }
        }

        let obj = value.as_object().ok_or_else(|| {
            <serde_json::Error as DeError>::custom("run event must be a JSON object")
        })?;
        let opt_str = |key: &str| obj.get(key).and_then(Value::as_str).map(str::to_string);
        let id = obj.get("id").and_then(Value::as_str).ok_or_else(|| {
            <serde_json::Error as DeError>::custom("missing or non-string field: id")
        })?;
        let ts = obj
            .get("ts")
            .ok_or_else(|| <serde_json::Error as DeError>::custom("missing field: ts"))
            .and_then(DateTime::<Utc>::deserialize)?;
        let run_id = obj
            .get("run_id")
            .ok_or_else(|| <serde_json::Error as DeError>::custom("missing field: run_id"))
            .and_then(RunId::deserialize)?;
        let event = obj.get("event").and_then(Value::as_str).ok_or_else(|| {
            <serde_json::Error as DeError>::custom("missing or non-string field: event")
        })?;
        let properties = obj
            .get("properties")
            .cloned()
            .unwrap_or_else(default_properties);
        Self::from_parts(RunEventParts {
            id: id.to_string(),
            ts,
            run_id,
            node_id: opt_str("node_id"),
            node_label: opt_str("node_label"),
            stage_id: opt_field(obj, "stage_id")?,
            parallel_group_id: opt_field(obj, "parallel_group_id")?,
            parallel_branch_id: opt_field(obj, "parallel_branch_id")?,
            session_id: opt_str("session_id"),
            parent_session_id: opt_str("parent_session_id"),
            tool_call_id: opt_str("tool_call_id"),
            actor: opt_field(obj, "actor")?,
            event,
            properties: &properties,
        })
    }

    fn from_parts(parts: RunEventParts<'_>) -> serde_json::Result<Self> {
        let body_payload = json!({
            "event": parts.event,
            "properties": parts.properties,
        });
        let body: EventBody = match serde_json::from_value(body_payload) {
            Ok(body) => body,
            Err(err) if is_known_event_name(parts.event) => return Err(err),
            Err(_) => EventBody::Unknown {
                name:       parts.event.to_string(),
                properties: parts.properties.clone(),
            },
        };
        Ok(Self {
            id: parts.id,
            ts: parts.ts,
            run_id: parts.run_id,
            node_id: parts.node_id,
            node_label: parts.node_label,
            stage_id: parts.stage_id,
            parallel_group_id: parts.parallel_group_id,
            parallel_branch_id: parts.parallel_branch_id,
            session_id: parts.session_id,
            parent_session_id: parts.parent_session_id,
            tool_call_id: parts.tool_call_id,
            actor: parts.actor,
            body,
        })
    }

    pub fn from_json_str(line: &str) -> serde_json::Result<Self> {
        Self::from_value(serde_json::from_str(line)?)
    }

    pub fn to_value(&self) -> serde_json::Result<Value> {
        fn insert_opt<T: Serialize>(
            map: &mut Map<String, Value>,
            key: &str,
            value: Option<&T>,
        ) -> serde_json::Result<()> {
            if let Some(v) = value {
                map.insert(key.to_string(), serde_json::to_value(v)?);
            }
            Ok(())
        }

        let mut map = Map::new();
        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert("ts".to_string(), serde_json::to_value(self.ts)?);
        map.insert("run_id".to_string(), serde_json::to_value(self.run_id)?);
        map.insert(
            "event".to_string(),
            Value::String(self.body.event_name().to_string()),
        );
        insert_opt(&mut map, "session_id", self.session_id.as_ref())?;
        insert_opt(
            &mut map,
            "parent_session_id",
            self.parent_session_id.as_ref(),
        )?;
        insert_opt(&mut map, "node_id", self.node_id.as_ref())?;
        insert_opt(&mut map, "node_label", self.node_label.as_ref())?;
        insert_opt(&mut map, "stage_id", self.stage_id.as_ref())?;
        insert_opt(
            &mut map,
            "parallel_group_id",
            self.parallel_group_id.as_ref(),
        )?;
        insert_opt(
            &mut map,
            "parallel_branch_id",
            self.parallel_branch_id.as_ref(),
        )?;
        insert_opt(&mut map, "tool_call_id", self.tool_call_id.as_ref())?;
        insert_opt(&mut map, "actor", self.actor.as_ref())?;
        map.insert("properties".to_string(), self.body.properties_value()?);
        Ok(Value::Object(map))
    }

    pub fn event_name(&self) -> &str {
        self.body.event_name()
    }

    pub fn properties(&self) -> serde_json::Result<Value> {
        self.body.properties_value()
    }
}

impl Serialize for RunEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RunEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::{
        AuthMethod, Edge, Graph, IdpIdentity, Node, PendingReason, RunBlobId, WorkflowSettings,
        fixtures,
    };

    fn user_principal(login: &str) -> Principal {
        Principal::user(
            IdpIdentity::new("https://github.com", "12345").unwrap(),
            login.to_string(),
            AuthMethod::Github,
        )
    }

    #[test]
    fn run_event_round_trips_json() {
        let event = RunEvent {
            id:                 "evt_1".to_string(),
            ts:                 DateTime::parse_from_rfc3339("2026-04-04T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id:             fixtures::RUN_1,
            node_id:            Some("build".to_string()),
            node_label:         Some("Build".to_string()),
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::StageCompleted(StageCompletedProps {
                index: 1,
                timing: crate::StageTiming::wall_only(1234),
                status: crate::StageOutcome::Succeeded,
                preferred_label: None,
                suggested_next_ids: vec!["next".to_string()],
                billing: None,
                failure: None,
                notes: Some("done".to_string()),
                files_touched: vec!["src/main.rs".to_string()],
                context_updates: None,
                jump_to_node: None,
                context_values: None,
                node_visits: None,
                loop_failure_signatures: None,
                restart_failure_signatures: None,
                response: None,
                attempt: 1,
                max_attempts: 1,
            }),
        };

        let value = event.to_value().unwrap();
        let parsed = RunEvent::from_value(value).unwrap();

        assert_eq!(parsed, event);
    }

    #[test]
    fn run_event_deserializes_adjacent_layout() {
        let settings = WorkflowSettings::default();
        let graph = Graph {
            name:  "test".to_string(),
            nodes: HashMap::from([("start".to_string(), Node {
                id:      "start".to_string(),
                attrs:   HashMap::new(),
                classes: Vec::new(),
            })]),
            edges: vec![Edge {
                from:  "start".to_string(),
                to:    "done".to_string(),
                attrs: HashMap::new(),
            }],
            attrs: HashMap::new(),
        };

        let line = json!({
            "id": "evt_2",
            "ts": "2026-04-04T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "run.created",
            "properties": {
                "settings": settings,
                "graph": graph,
                "labels": {},
                "run_dir": "/tmp/run",
                "source_directory": "/tmp/run"
            }
        });

        let parsed = RunEvent::from_value(line).unwrap();
        assert!(matches!(parsed.body, EventBody::RunCreated(_)));
    }

    #[test]
    fn run_created_round_trip_preserves_manifest_blob() {
        let line = json!({
            "id": "evt_created_blob",
            "ts": "2026-04-04T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "run.created",
            "properties": {
                "settings": WorkflowSettings::default(),
                "graph": Graph::new("test"),
                "labels": {},
                "run_dir": "/tmp/run",
                "source_directory": "/tmp/run",
                "manifest_blob": RunBlobId::new(br#"{"version":1}"#).to_string()
            }
        });

        let parsed = RunEvent::from_value(line.clone()).unwrap();
        let serialized = parsed.to_value().unwrap();

        assert_eq!(
            serialized["properties"]["manifest_blob"],
            line["properties"]["manifest_blob"]
        );
    }

    #[test]
    fn interview_interrupted_kind_matches_event_name() {
        let body = EventBody::InterviewInterrupted(InterviewInterruptedProps {
            question_id: "q-1".to_string(),
            question:    "approve?".to_string(),
            stage:       "gate".to_string(),
            reason:      "interrupted".to_string(),
            duration_ms: 12,
        });

        assert_eq!(body.event_name(), "interview.interrupted");
    }

    #[test]
    fn run_interrupt_round_trips_with_empty_properties_and_actor() {
        let line = json!({
            "id": "evt_interrupt",
            "ts": "2026-04-04T12:00:00Z",
            "run_id": fixtures::RUN_1,
            "event": "run.interrupt",
            "actor": { "kind": "system", "system_kind": "engine" },
            "properties": {}
        });

        let parsed = RunEvent::from_value(line.clone()).unwrap();
        assert!(matches!(parsed.body, EventBody::RunInterrupt(_)));
        assert_eq!(parsed.to_value().unwrap(), line);
    }

    #[test]
    fn run_steer_round_trips_with_text_and_actor() {
        let line = json!({
            "id": "evt_steer",
            "ts": "2026-04-04T12:00:00Z",
            "run_id": fixtures::RUN_1,
            "event": "run.steer",
            "actor": { "kind": "system", "system_kind": "engine" },
            "properties": { "text": "try another approach" }
        });

        let parsed = RunEvent::from_value(line.clone()).unwrap();
        assert!(matches!(
            &parsed.body,
            EventBody::RunSteer(props) if props.text == "try another approach"
        ));
        assert_eq!(parsed.to_value().unwrap(), line);
    }

    #[test]
    fn pre_execution_lifecycle_events_round_trip() {
        let cases = [
            (
                EventBody::RunStartRequested(RunStartRequestedProps { resume: false }),
                json!("run.start_requested"),
                json!({ "resume": false }),
            ),
            (
                EventBody::RunPending(RunPendingProps {
                    reason: PendingReason::ApprovalRequired,
                }),
                json!("run.pending"),
                json!({ "reason": "approval_required" }),
            ),
            (
                EventBody::RunApproved(RunApprovedProps::default()),
                json!("run.approved"),
                json!({}),
            ),
            (
                EventBody::RunDenied(RunDeniedProps {
                    reason: Some("Not approved for execution".to_string()),
                }),
                json!("run.denied"),
                json!({ "reason": "Not approved for execution" }),
            ),
            (
                EventBody::RunRunnable(RunRunnableProps {
                    source: RunRunnableSource::Approved,
                }),
                json!("run.runnable"),
                json!({ "source": "approved" }),
            ),
        ];

        for (body, event_name, properties) in cases {
            let event = RunEvent {
                id: format!("evt_{}", event_name.as_str().unwrap()),
                ts: DateTime::parse_from_rfc3339("2026-05-23T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                run_id: fixtures::RUN_1,
                node_id: None,
                node_label: None,
                stage_id: None,
                parallel_group_id: None,
                parallel_branch_id: None,
                session_id: None,
                parent_session_id: None,
                tool_call_id: None,
                actor: Some(Principal::System {
                    system_kind: crate::SystemActorKind::Engine,
                }),
                body,
            };
            let value = event.to_value().unwrap();
            assert_eq!(value["event"], event_name);
            assert_eq!(value["properties"], properties);
            assert_eq!(RunEvent::from_value(value).unwrap(), event);
        }
    }

    #[test]
    fn agent_interrupt_injected_round_trips_with_stage_session_and_actor() {
        let line = json!({
            "id": "evt_interrupt_injected",
            "ts": "2026-04-04T12:00:00Z",
            "run_id": fixtures::RUN_1,
            "event": "agent.interrupt.injected",
            "node_id": "code",
            "node_label": "code",
            "stage_id": "code@2",
            "session_id": "ses_1",
            "actor": { "kind": "system", "system_kind": "engine" },
            "properties": { "visit": 2 }
        });

        let parsed = RunEvent::from_value(line.clone()).unwrap();
        assert!(matches!(
            &parsed.body,
            EventBody::AgentInterruptInjected(props) if props.visit == 2
        ));
        assert_eq!(parsed.to_value().unwrap(), line);
    }

    #[test]
    fn run_interrupt_then_steer_is_not_a_known_persisted_event() {
        let line = json!({
            "id": "evt_combined",
            "ts": "2026-04-04T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "run.interrupt_then_steer",
            "properties": { "text": "try another approach" }
        });

        let parsed = RunEvent::from_value(line).unwrap();
        assert!(matches!(
            parsed.body,
            EventBody::Unknown { ref name, .. } if name == "run.interrupt_then_steer"
        ));
    }

    #[test]
    fn patch_bearing_events_round_trip_diff_summary() {
        for (event_name, properties) in [
            (
                "checkpoint.completed",
                json!({
                    "status": "running",
                    "current_node": "build",
                    "completed_nodes": ["build"],
                    "diff_summary": {
                        "files_changed": 2,
                        "additions": 10,
                        "deletions": 3
                    }
                }),
            ),
            (
                "run.completed",
                json!({
                    "timing": {
                        "wall_time_ms": 42,
                        "inference_time_ms": 0,
                        "tool_time_ms": 0,
                        "active_time_ms": 0
                    },
                    "artifact_count": 0,
                    "status": "succeeded",
                    "reason": "completed",
                    "diff_summary": {
                        "files_changed": 2,
                        "additions": 10,
                        "deletions": 3
                    }
                }),
            ),
            (
                "run.failed",
                json!({
                    "failure": {
                        "reason": "workflow_error",
                        "detail": {
                            "message": "boom",
                            "category": "deterministic"
                        }
                    },
                    "timing": {
                        "wall_time_ms": 42,
                        "inference_time_ms": 0,
                        "tool_time_ms": 0,
                        "active_time_ms": 0
                    },
                    "diff_summary": {
                        "files_changed": 2,
                        "additions": 10,
                        "deletions": 3
                    }
                }),
            ),
        ] {
            let line = json!({
                "id": format!("evt_{event_name}"),
                "ts": "2026-04-04T12:00:00Z",
                "run_id": fixtures::RUN_1,
                "event": event_name,
                "node_id": "build",
                "properties": properties
            });

            let parsed = RunEvent::from_value(line).unwrap();
            let serialized = parsed.to_value().unwrap();

            assert_eq!(
                serialized["properties"]["diff_summary"],
                json!({
                    "files_changed": 2,
                    "additions": 10,
                    "deletions": 3
                }),
                "{event_name} should preserve diff_summary"
            );
        }
    }

    #[test]
    fn run_submitted_round_trip_preserves_definition_blob() {
        let line = json!({
            "id": "evt_submitted_blob",
            "ts": "2026-04-04T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "run.submitted",
            "properties": {
                "definition_blob": RunBlobId::new(br#"{"workflow_path":"workflow.fabro"}"#).to_string()
            }
        });

        let parsed = RunEvent::from_value(line.clone()).unwrap();
        let serialized = parsed.to_value().unwrap();

        assert_eq!(
            serialized["properties"]["definition_blob"],
            line["properties"]["definition_blob"]
        );
    }

    #[test]
    fn event_body_event_name_matches_wire_name() {
        let body = EventBody::StageCompleted(StageCompletedProps {
            index: 1,
            timing: crate::StageTiming::wall_only(1234),
            status: crate::StageOutcome::Succeeded,
            preferred_label: None,
            suggested_next_ids: vec!["next".to_string()],
            billing: None,
            failure: None,
            notes: Some("done".to_string()),
            files_touched: vec!["src/main.rs".to_string()],
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            response: None,
            attempt: 1,
            max_attempts: 1,
        });

        assert_eq!(body.event_name(), "stage.completed");
    }

    #[test]
    fn run_event_preserves_unknown_event_name_and_properties() {
        let value = json!({
            "id": "evt_unknown",
            "ts": "2026-04-04T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "vendor.custom.event",
            "properties": {
                "answer": 42,
                "nested": { "ok": true }
            }
        });

        let parsed = RunEvent::from_value(value.clone()).unwrap();
        let serialized = parsed.to_value().unwrap();

        assert_eq!(parsed.event_name(), "vendor.custom.event");
        assert_eq!(parsed.properties().unwrap(), value["properties"]);
        assert_eq!(serialized["event"], value["event"]);
        assert_eq!(serialized["properties"], value["properties"]);
    }

    #[test]
    fn run_event_round_trips_new_envelope_fields() {
        let value = json!({
            "id": "evt_envelope",
            "ts": "2026-04-08T16:21:11.106Z",
            "run_id": fixtures::RUN_1,
            "event": "agent.tool.completed",
            "stage_id": "code@1",
            "node_id": "code",
            "node_label": "Code",
            "parallel_group_id": "code@1",
            "parallel_branch_id": "code@1:0",
            "session_id": "ses_child",
            "parent_session_id": "ses_parent",
            "tool_call_id": "call_1",
            "actor": {
                "kind": "agent",
                "session_id": "ses_child",
                "parent_session_id": "ses_parent",
                "model": "claude-sonnet"
            },
            "properties": {
                "tool_name": "read_file",
                "tool_call_id": "call_1",
                "output": {"summary": "read"},
                "is_error": false,
                "visit": 1
            }
        });

        let parsed = RunEvent::from_value(value.clone()).unwrap();
        assert_eq!(parsed.stage_id, Some(StageId::new("code", 1)));
        assert_eq!(parsed.parallel_group_id, Some(StageId::new("code", 1)));
        assert_eq!(
            parsed.parallel_branch_id,
            Some(ParallelBranchId::new(StageId::new("code", 1), 0))
        );
        assert_eq!(parsed.tool_call_id.as_deref(), Some("call_1"));
        let actor = parsed.actor.as_ref().expect("actor present");
        assert_eq!(actor, &Principal::Agent {
            session_id:        Some("ses_child".to_string()),
            parent_session_id: Some("ses_parent".to_string()),
            model:             Some("claude-sonnet".to_string()),
        });

        let serialized = parsed.to_value().unwrap();
        assert_eq!(serialized["stage_id"], value["stage_id"]);
        assert_eq!(serialized["parallel_group_id"], value["parallel_group_id"]);
        assert_eq!(
            serialized["parallel_branch_id"],
            value["parallel_branch_id"]
        );
        assert_eq!(serialized["tool_call_id"], value["tool_call_id"]);
        assert_eq!(serialized["actor"], value["actor"]);
    }

    #[test]
    fn agent_session_ended_serializes_empty_properties() {
        let event = RunEvent {
            id:                 "evt_session_ended".to_string(),
            ts:                 DateTime::parse_from_rfc3339("2026-04-04T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id:             fixtures::RUN_1,
            node_id:            None,
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         Some("ses_abc".to_string()),
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::AgentSessionEnded(AgentSessionEndedProps {}),
        };

        let serialized = event.to_value().unwrap();

        assert_eq!(serialized["event"], "agent.session.ended");
        assert_eq!(serialized["session_id"], "ses_abc");
        assert_eq!(serialized["properties"], json!({}));
        assert!(serialized.get("node_id").is_none());
        assert!(serialized.get("stage_id").is_none());
    }

    #[test]
    fn run_event_omits_absent_envelope_fields() {
        let event = RunEvent {
            id:                 "evt_bare".to_string(),
            ts:                 DateTime::parse_from_rfc3339("2026-04-04T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id:             fixtures::RUN_1,
            node_id:            None,
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::RunStarted(RunStartedProps {
                name:         "demo".to_string(),
                base_branch:  None,
                base_sha:     None,
                run_branch:   None,
                worktree_dir: None,
                goal:         None,
            }),
        };

        let serialized = event.to_value().unwrap();
        let obj = serialized.as_object().unwrap();
        assert!(!obj.contains_key("stage_id"));
        assert!(!obj.contains_key("parallel_group_id"));
        assert!(!obj.contains_key("parallel_branch_id"));
        assert!(!obj.contains_key("tool_call_id"));
        assert!(!obj.contains_key("actor"));
    }

    #[test]
    fn canonical_run_lifecycle_events_are_known() {
        for event in [
            "run.start_requested",
            "run.pending",
            "run.approved",
            "run.denied",
            "run.runnable",
            "run.blocked",
            "run.unblocked",
        ] {
            assert!(
                is_known_event_name(event),
                "{event} should be a known event"
            );
        }
    }

    #[test]
    fn run_blocked_round_trips_as_typed_event() {
        let value = json!({
            "id": "evt_run_blocked",
            "ts": "2026-04-19T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "run.blocked",
            "properties": {
                "blocked_reason": "human_input_required"
            }
        });

        let parsed = RunEvent::from_value(value.clone()).unwrap();
        assert!(
            !matches!(parsed.body, EventBody::Unknown { .. }),
            "run.blocked should deserialize into a typed event body"
        );

        let serialized = parsed.to_value().unwrap();
        assert_eq!(serialized["event"], "run.blocked");
        assert_eq!(
            serialized["properties"]["blocked_reason"],
            value["properties"]["blocked_reason"]
        );
    }

    #[test]
    fn run_archived_serializes_with_dotted_event_name_without_actor_property() {
        let body = EventBody::RunArchived(RunArchivedProps::default());
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "run.archived");
        assert_eq!(value["properties"], json!({}));
    }

    #[test]
    fn run_unarchived_serializes_without_actor_property() {
        let body = EventBody::RunUnarchived(RunUnarchivedProps::default());
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "run.unarchived");
        assert_eq!(value["properties"], json!({}));
    }

    #[test]
    fn run_archived_round_trips_through_from_value() {
        let value = json!({
            "id": "evt_archived",
            "ts": "2026-04-19T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "run.archived",
            "actor": {
                "kind": "user",
                "identity": {
                    "issuer": "https://github.com",
                    "subject": "12345"
                },
                "login": "alice",
                "auth_method": "github"
            },
            "properties": {}
        });

        let parsed = RunEvent::from_value(value.clone()).unwrap();
        assert!(matches!(parsed.body, EventBody::RunArchived(_)));
        assert_eq!(parsed.actor, Some(user_principal("alice")));
        let serialized = parsed.to_value().unwrap();
        assert_eq!(serialized["event"], "run.archived");
        assert_eq!(serialized["actor"], value["actor"]);
        assert_eq!(serialized["properties"], json!({}));
    }

    #[test]
    fn run_unarchived_round_trips_through_from_value() {
        let value = json!({
            "id": "evt_unarchived",
            "ts": "2026-04-19T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "run.unarchived",
            "properties": {}
        });

        let parsed = RunEvent::from_value(value.clone()).unwrap();
        match &parsed.body {
            EventBody::RunUnarchived(_) => {}
            other => panic!("expected RunUnarchived body, got {other:?}"),
        }
    }

    #[test]
    fn run_runnable_and_unblocked_round_trip_as_typed_events() {
        for value in [
            json!({
                "id": "evt_run_runnable",
                "ts": "2026-04-19T12:00:00.000Z",
                "run_id": fixtures::RUN_1,
                "event": "run.runnable",
                "properties": { "source": "start_requested" }
            }),
            json!({
                "id": "evt_run_unblocked",
                "ts": "2026-04-19T12:00:00.000Z",
                "run_id": fixtures::RUN_1,
                "event": "run.unblocked",
                "properties": {}
            }),
        ] {
            let parsed = RunEvent::from_value(value.clone()).unwrap();
            assert!(
                !matches!(parsed.body, EventBody::Unknown { .. }),
                "{} should deserialize into a typed event body",
                value["event"].as_str().unwrap()
            );
            assert_eq!(parsed.to_value().unwrap()["event"], value["event"]);
        }
    }

    #[test]
    fn metadata_snapshot_events_are_known_and_round_trip_json() {
        let completed = RunEvent {
            id:                 "evt_metadata_completed".to_string(),
            ts:                 DateTime::parse_from_rfc3339("2026-04-29T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id:             fixtures::RUN_1,
            node_id:            None,
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::MetadataSnapshotCompleted(
                MetadataSnapshotCompletedProps {
                    phase:       MetadataSnapshotPhase::Checkpoint,
                    branch:      "fabro/metadata/run".to_string(),
                    duration_ms: 2800,
                    entry_count: 3,
                    bytes:       42,
                    commit_sha:  "abc123".to_string(),
                },
            ),
        };

        let serialized = completed.to_value().unwrap();
        assert_eq!(serialized["event"], "metadata.snapshot.completed");
        assert_eq!(serialized["properties"]["phase"], "checkpoint");
        assert_eq!(serialized["properties"]["branch"], "fabro/metadata/run");
        assert_eq!(serialized["properties"]["duration_ms"], 2800);
        assert_eq!(serialized["properties"]["entry_count"], 3);
        assert_eq!(serialized["properties"]["bytes"], 42);
        assert_eq!(serialized["properties"]["commit_sha"], "abc123");

        let parsed = RunEvent::from_value(serialized).unwrap();
        assert_eq!(parsed.event_name(), "metadata.snapshot.completed");
        assert!(matches!(
            parsed.body,
            EventBody::MetadataSnapshotCompleted(MetadataSnapshotCompletedProps {
                phase: MetadataSnapshotPhase::Checkpoint,
                ..
            })
        ));
    }

    #[test]
    fn pull_request_linked_round_trips_json() {
        let event = RunEvent {
            id:                 "evt_pr_linked".to_string(),
            ts:                 DateTime::parse_from_rfc3339("2026-05-15T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id:             fixtures::RUN_1,
            node_id:            None,
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::PullRequestLinked(PullRequestLinkedProps {
                pull_request: crate::PullRequestLink {
                    owner:  "acme".to_string(),
                    repo:   "widgets".to_string(),
                    number: 42,
                },
            }),
        };

        let value = event.to_value().unwrap();
        assert_eq!(value["event"], "pull_request.linked");
        assert_eq!(
            value["properties"]["pull_request"]["html_url"],
            "https://github.com/acme/widgets/pull/42"
        );

        let parsed = RunEvent::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn pull_request_unlinked_round_trips_json() {
        let event = RunEvent {
            id:                 "evt_pr_unlinked".to_string(),
            ts:                 DateTime::parse_from_rfc3339("2026-05-15T12:05:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id:             fixtures::RUN_1,
            node_id:            None,
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::PullRequestUnlinked(PullRequestUnlinkedProps {
                pull_request: crate::PullRequestLink {
                    owner:  "acme".to_string(),
                    repo:   "widgets".to_string(),
                    number: 42,
                },
            }),
        };

        let value = event.to_value().unwrap();
        assert_eq!(value["event"], "pull_request.unlinked");
        assert_eq!(
            value["properties"]["pull_request"]["number"],
            serde_json::json!(42)
        );

        let parsed = RunEvent::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn retired_sandbox_snapshot_events_deserialize_as_unknown() {
        for (event_name, expected_properties) in [
            (
                "sandbox.snapshot.pulled",
                json!({"name": "buildpack-deps:noble", "duration_ms": 5000}),
            ),
            ("sandbox.snapshot.ensuring", json!({"name": "fabro-v8"})),
        ] {
            let value = json!({
                "id": "evt_retired_snapshot",
                "ts": "2026-04-29T12:00:00.000Z",
                "run_id": fixtures::RUN_1,
                "event": event_name,
                "properties": expected_properties
            });

            let parsed = RunEvent::from_value(value).unwrap();
            match parsed.body {
                EventBody::Unknown { name, properties } => {
                    assert_eq!(name, event_name);
                    assert_eq!(properties, expected_properties);
                }
                other => panic!("expected Unknown body, got {other:?}"),
            }
        }
    }

    #[test]
    fn retired_retro_events_deserialize_as_unknown() {
        for (event_name, expected_properties) in [
            (
                "retro.started",
                json!({"prompt": "Analyze the run", "provider": "openai", "model": "gpt-5"}),
            ),
            (
                "retro.completed",
                json!({"duration_ms": 1200, "response": "done", "retro": {"smoothness": "smooth"}}),
            ),
            (
                "retro.failed",
                json!({"duration_ms": 1200, "error": "state unavailable"}),
            ),
        ] {
            let value = json!({
                "id": "evt_retired_retro",
                "ts": "2026-05-08T12:00:00.000Z",
                "run_id": fixtures::RUN_1,
                "event": event_name,
                "properties": expected_properties
            });

            let parsed = RunEvent::from_value(value).unwrap();
            match parsed.body {
                EventBody::Unknown { name, properties } => {
                    assert_eq!(name, event_name);
                    assert_eq!(properties, expected_properties);
                }
                other => panic!("expected Unknown body, got {other:?}"),
            }
        }
    }

    #[test]
    fn metadata_snapshot_failed_omits_empty_optional_fields() {
        let body = EventBody::MetadataSnapshotFailed(MetadataSnapshotFailedProps {
            phase:            MetadataSnapshotPhase::Init,
            branch:           "fabro/metadata/run".to_string(),
            duration_ms:      15,
            failure_kind:     MetadataSnapshotFailureKind::LoadState,
            error:            "state unavailable".to_string(),
            causes:           Vec::new(),
            commit_sha:       None,
            entry_count:      None,
            bytes:            None,
            exec_output_tail: None,
        });

        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "metadata.snapshot.failed");
        assert_eq!(
            value["properties"],
            json!({
                "phase": "init",
                "branch": "fabro/metadata/run",
                "duration_ms": 15,
                "failure_kind": "load_state",
                "error": "state unavailable"
            })
        );
    }

    #[test]
    fn metadata_snapshot_failed_serializes_exec_output_tail_additively() {
        let body = EventBody::MetadataSnapshotFailed(MetadataSnapshotFailedProps {
            phase:            MetadataSnapshotPhase::Checkpoint,
            branch:           "fabro/metadata/run".to_string(),
            duration_ms:      20,
            failure_kind:     MetadataSnapshotFailureKind::Push,
            error:            "push failed".to_string(),
            causes:           Vec::new(),
            commit_sha:       None,
            entry_count:      None,
            bytes:            None,
            exec_output_tail: Some(ExecOutputTail {
                stdout:           Some("last stdout line".to_string()),
                stderr:           Some("last stderr line".to_string()),
                stdout_truncated: false,
                stderr_truncated: true,
            }),
        });

        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(
            value["properties"]["exec_output_tail"]["stdout"],
            "last stdout line"
        );
        assert_eq!(
            value["properties"]["exec_output_tail"]["stderr"],
            "last stderr line"
        );
        assert_eq!(
            value["properties"]["exec_output_tail"]["stderr_truncated"],
            true
        );
        assert!(
            value["properties"]["exec_output_tail"]
                .as_object()
                .expect("exec output tail object")
                .get("stdout_truncated")
                .is_none()
        );

        let body_without_tail = EventBody::MetadataSnapshotFailed(MetadataSnapshotFailedProps {
            phase:            MetadataSnapshotPhase::Checkpoint,
            branch:           "fabro/metadata/run".to_string(),
            duration_ms:      20,
            failure_kind:     MetadataSnapshotFailureKind::Push,
            error:            "push failed".to_string(),
            causes:           Vec::new(),
            commit_sha:       None,
            entry_count:      None,
            bytes:            None,
            exec_output_tail: None,
        });
        let value_without_tail = serde_json::to_value(&body_without_tail).unwrap();
        assert!(
            value_without_tail["properties"]
                .as_object()
                .expect("properties object")
                .get("exec_output_tail")
                .is_none()
        );
    }

    #[test]
    fn exec_output_tail_fields_are_additive_on_failure_props() {
        let tail = ExecOutputTail {
            stdout:           Some("last stdout line".to_string()),
            stderr:           Some("last stderr line".to_string()),
            stdout_truncated: false,
            stderr_truncated: true,
        };

        for body in [
            EventBody::RunNotice(RunNoticeProps {
                level:            RunNoticeLevel::Warn,
                code:             RunNoticeCode::GitDiffFailed.to_string(),
                message:          "git diff failed".to_string(),
                exec_output_tail: Some(tail.clone()),
            }),
            EventBody::CheckpointFailed(CheckpointFailedProps {
                error:            "git commit failed".to_string(),
                exec_output_tail: Some(tail.clone()),
            }),
            EventBody::GitPush(GitPushProps {
                branch:           "refs/heads/run:refs/heads/run".to_string(),
                success:          false,
                exec_output_tail: Some(tail.clone()),
            }),
        ] {
            let value = serde_json::to_value(&body).unwrap();
            assert_eq!(
                value["properties"]["exec_output_tail"]["stderr"],
                "last stderr line"
            );
            assert_eq!(
                value["properties"]["exec_output_tail"]["stderr_truncated"],
                true
            );
        }
    }

    #[test]
    fn absent_exec_output_tail_is_omitted_from_new_failure_props() {
        for body in [
            EventBody::RunNotice(RunNoticeProps {
                level:            RunNoticeLevel::Warn,
                code:             RunNoticeCode::GitDiffFailed.to_string(),
                message:          "git diff failed".to_string(),
                exec_output_tail: None,
            }),
            EventBody::CheckpointFailed(CheckpointFailedProps {
                error:            "git commit failed".to_string(),
                exec_output_tail: None,
            }),
            EventBody::GitPush(GitPushProps {
                branch:           "refs/heads/run:refs/heads/run".to_string(),
                success:          false,
                exec_output_tail: None,
            }),
        ] {
            let value = serde_json::to_value(&body).unwrap();
            assert!(
                value["properties"]
                    .as_object()
                    .expect("properties object")
                    .get("exec_output_tail")
                    .is_none()
            );
        }
    }

    #[test]
    fn todo_event_names_are_known() {
        for name in ["todo.created", "todo.updated", "todo.deleted"] {
            assert!(is_known_event_name(name), "{name} should be a known event");
        }
    }

    #[test]
    fn todo_created_serializes_with_canonical_name() {
        let body = EventBody::TodoCreated(TodoCreatedProps {
            list_id:     "openai_plan:ses_1".to_string(),
            list_kind:   crate::TodoListKind::OpenAiPlan,
            todo_id:     "todo_1".to_string(),
            status:      crate::TodoStatus::Pending,
            order:       0,
            subject:     "do the thing".to_string(),
            description: String::new(),
            active_form: None,
            owner:       None,
            blocks:      Vec::new(),
            blocked_by:  Vec::new(),
            metadata:    std::collections::BTreeMap::new(),
        });

        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "todo.created");
        assert_eq!(value["properties"]["list_id"], "openai_plan:ses_1");
        assert_eq!(value["properties"]["todo_id"], "todo_1");
        assert_eq!(value["properties"]["status"], "pending");
        assert_eq!(value["properties"]["subject"], "do the thing");
        // Optional fields are omitted.
        let props = value["properties"].as_object().unwrap();
        assert!(!props.contains_key("description"));
        assert!(!props.contains_key("active_form"));
        assert!(!props.contains_key("metadata"));
    }

    #[test]
    fn todo_envelope_includes_session_metadata() {
        let event = RunEvent {
            id:                 "evt_todo".to_string(),
            ts:                 DateTime::parse_from_rfc3339("2026-05-22T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id:             fixtures::RUN_1,
            node_id:            Some("code".to_string()),
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         Some("ses_child".to_string()),
            parent_session_id:  Some("ses_parent".to_string()),
            tool_call_id:       Some("call_xyz".to_string()),
            actor:              None,
            body:               EventBody::TodoUpdated(TodoUpdatedProps {
                list_id:        "anthropic_tasks:ses_root".to_string(),
                list_kind:      crate::TodoListKind::AnthropicTasks,
                todo_id:        "42".to_string(),
                status:         Some(crate::TodoStatus::InProgress),
                order:          None,
                subject:        None,
                description:    None,
                active_form:    None,
                owner:          None,
                add_blocks:     None,
                add_blocked_by: None,
                metadata_patch: std::collections::BTreeMap::new(),
            }),
        };

        let value = event.to_value().unwrap();
        assert_eq!(value["event"], "todo.updated");
        assert_eq!(value["session_id"], "ses_child");
        assert_eq!(value["parent_session_id"], "ses_parent");
        assert_eq!(value["tool_call_id"], "call_xyz");
        assert_eq!(value["properties"]["list_id"], "anthropic_tasks:ses_root");
        assert_eq!(value["properties"]["status"], "in_progress");

        let parsed = RunEvent::from_value(value).unwrap();
        match &parsed.body {
            EventBody::TodoUpdated(props) => {
                assert_eq!(props.status, Some(crate::TodoStatus::InProgress));
            }
            other => panic!("expected TodoUpdated body, got {other:?}"),
        }
    }

    #[test]
    fn todo_deleted_round_trips() {
        let body = EventBody::TodoDeleted(TodoDeletedProps {
            list_id:   "openai_plan:ses_1".to_string(),
            list_kind: crate::TodoListKind::OpenAiPlan,
            todo_id:   "todo_x".to_string(),
        });

        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "todo.deleted");
        assert_eq!(value["properties"]["todo_id"], "todo_x");

        let parsed: EventBody = serde_json::from_value(value).unwrap();
        match parsed {
            EventBody::TodoDeleted(props) => assert_eq!(props.todo_id, "todo_x"),
            other => panic!("expected TodoDeleted, got {other:?}"),
        }
    }

    #[test]
    fn agent_memory_loaded_serializes_with_canonical_name() {
        let body = EventBody::AgentMemoryLoaded(AgentMemoryLoadedProps {
            provider_profile:   "anthropic".to_string(),
            files:              vec![AgentMemoryFileProps {
                path:         "/repo/AGENTS.md".to_string(),
                byte_count:   100,
                loaded_bytes: 100,
                truncated:    false,
            }],
            total_loaded_bytes: 100,
            budget_bytes:       32768,
            visit:              1,
        });
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "agent.memory.loaded");
        assert_eq!(value["properties"]["provider_profile"], "anthropic");
        assert_eq!(value["properties"]["files"][0]["path"], "/repo/AGENTS.md");
        assert_eq!(value["properties"]["budget_bytes"], 32768);
        assert!(
            value["properties"]
                .as_object()
                .unwrap()
                .get("content")
                .is_none(),
            "memory event must not include file content"
        );
        let _ = serde_json::from_value::<EventBody>(value).unwrap();
    }

    #[test]
    fn agent_skills_discovered_serializes_with_canonical_name() {
        let body = EventBody::AgentSkillsDiscovered(AgentSkillsDiscoveredProps {
            provider_profile: "openai".to_string(),
            source_dirs:      vec!["/repo/.fabro/skills".to_string()],
            skills:           vec![AgentSkillSummary {
                name:        "commit".to_string(),
                description: "Create a commit".to_string(),
            }],
            visit:            2,
        });
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "agent.skills.discovered");
        assert_eq!(value["properties"]["skills"][0]["name"], "commit");
        let _: EventBody = serde_json::from_value(value).unwrap();
    }

    #[test]
    fn agent_skill_activated_serializes_source_variants() {
        let slash = EventBody::AgentSkillActivated(AgentSkillActivatedProps {
            skill_name: "commit".to_string(),
            source:     AgentSkillActivationSource::Slash,
            visit:      3,
        });
        let value = serde_json::to_value(&slash).unwrap();
        assert_eq!(value["event"], "agent.skill.activated");
        assert_eq!(value["properties"]["source"], "slash");

        let tool = EventBody::AgentSkillActivated(AgentSkillActivatedProps {
            skill_name: "commit".to_string(),
            source:     AgentSkillActivationSource::Tool,
            visit:      4,
        });
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(value["properties"]["source"], "tool");
    }

    #[test]
    fn agent_context_window_snapshot_serializes_with_canonical_name() {
        let body = EventBody::AgentContextWindowSnapshot(AgentContextWindowSnapshotProps {
            stage_id: crate::StageId::new("implement", 1),
            visit:    1,
            snapshot: crate::StageContextWindowProjection {
                provider:              "openai".to_string(),
                model:                 "gpt-5.4".to_string(),
                context_window_tokens: 400_000,
                input_tokens:          123_456,
                usage_percent:         30.864,
                count_method:
                    crate::StageContextWindowCountMethod::ProviderApiScaledBreakdown,
                staleness:             crate::StageContextWindowStaleness::Live,
                generated_at:          DateTime::parse_from_rfc3339("2026-05-23T12:34:56Z")
                    .unwrap()
                    .with_timezone(&Utc),
                event_seq:             None,
                breakdown:             vec![crate::StageContextWindowBreakdownItem {
                    category:      crate::StageContextWindowCategory::SystemPrompt,
                    tokens:        30_000,
                    usage_percent: 7.5,
                }],
                warnings:              vec![crate::StageContextWindowWarning {
                    code:    "local_token_estimate".to_string(),
                    message: "input token count is a local estimate".to_string(),
                }],
            },
        });
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "agent.context_window.snapshot");
        assert_eq!(value["properties"]["stage_id"], "implement@1");
        assert_eq!(
            value["properties"]["breakdown"][0]["category"],
            "system_prompt"
        );
        let parsed: EventBody = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.event_name(), "agent.context_window.snapshot");
    }

    #[test]
    fn agent_mcp_ready_deserializes_legacy_payload_without_tools() {
        let value = json!({
            "id": "evt_mcp_ready",
            "ts": "2026-05-22T12:00:00.000Z",
            "run_id": fixtures::RUN_1,
            "event": "agent.mcp.ready",
            "properties": {
                "server_name": "github",
                "tool_count": 2,
                "visit": 1
            }
        });
        let parsed = RunEvent::from_value(value).unwrap();
        match parsed.body {
            EventBody::AgentMcpReady(props) => {
                assert_eq!(props.server_name, "github");
                assert_eq!(props.tool_count, 2);
                assert!(props.tools.is_empty());
                assert_eq!(props.visit, 1);
            }
            other => panic!("expected AgentMcpReady body, got {other:?}"),
        }
    }

    #[test]
    fn agent_mcp_ready_serializes_with_tool_summaries() {
        let body = EventBody::AgentMcpReady(AgentMcpReadyProps {
            server_name: "github".to_string(),
            tool_count:  1,
            tools:       vec![AgentMcpToolSummary {
                name:          "mcp__github__create_issue".to_string(),
                original_name: "create_issue".to_string(),
            }],
            visit:       1,
        });
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "agent.mcp.ready");
        assert_eq!(
            value["properties"]["tools"][0]["name"],
            "mcp__github__create_issue"
        );
        assert_eq!(
            value["properties"]["tools"][0]["original_name"],
            "create_issue"
        );
    }

    #[test]
    fn agent_mcp_ready_omits_tools_when_empty() {
        let body = EventBody::AgentMcpReady(AgentMcpReadyProps {
            server_name: "github".to_string(),
            tool_count:  0,
            tools:       Vec::new(),
            visit:       1,
        });
        let value = serde_json::to_value(&body).unwrap();
        assert!(
            value["properties"]
                .as_object()
                .unwrap()
                .get("tools")
                .is_none(),
            "empty tools should be omitted for legacy parity"
        );
    }
}
