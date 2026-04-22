pub mod agent;
pub mod infra;
pub mod misc;
pub mod run;
pub mod stage;

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
pub use stage::*;

use crate::{ParallelBranchId, RunId, StageId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunNoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRef {
    pub kind:    ActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

impl ActorRef {
    #[must_use]
    pub fn user(login: String) -> Self {
        Self {
            kind:    ActorKind::User,
            id:      Some(login.clone()),
            display: Some(login),
        }
    }

    #[must_use]
    pub fn agent(session_id: Option<String>, display: Option<String>) -> Self {
        Self {
            kind: ActorKind::Agent,
            id: session_id,
            display,
        }
    }
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
    pub actor:              Option<ActorRef>,
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
    #[serde(rename = "run.queued")]
    RunQueued(RunStatusEffectProps),
    #[serde(rename = "run.starting")]
    RunStarting(RunStatusTransitionProps),
    #[serde(rename = "run.running")]
    RunRunning(RunStatusTransitionProps),
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
    #[serde(rename = "run.rewound")]
    RunRewound(RunRewoundProps),
    #[serde(rename = "run.archived")]
    RunArchived(RunArchivedProps),
    #[serde(rename = "run.unarchived")]
    RunUnarchived(RunUnarchivedProps),
    #[serde(rename = "run.completed")]
    RunCompleted(RunCompletedProps),
    #[serde(rename = "run.failed")]
    RunFailed(RunFailedProps),
    #[serde(rename = "run.notice")]
    RunNotice(RunNoticeProps),
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
    #[serde(rename = "agent.compaction.started")]
    AgentCompactionStarted(AgentCompactionStartedProps),
    #[serde(rename = "agent.compaction.completed")]
    AgentCompactionCompleted(AgentCompactionCompletedProps),
    #[serde(rename = "agent.llm.retry")]
    AgentLlmRetry(AgentLlmRetryProps),
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
    #[serde(rename = "sandbox.snapshot.pulling")]
    SnapshotPulling(SnapshotNameProps),
    #[serde(rename = "sandbox.snapshot.pulled")]
    SnapshotPulled(SnapshotCompletedProps),
    #[serde(rename = "sandbox.snapshot.ensuring")]
    SnapshotEnsuring(SnapshotNameProps),
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
    #[serde(rename = "agent.cli.started")]
    AgentCliStarted(AgentCliStartedProps),
    #[serde(rename = "agent.cli.completed")]
    AgentCliCompleted(AgentCliCompletedProps),
    #[serde(rename = "pull_request.created")]
    PullRequestCreated(PullRequestCreatedProps),
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
    #[serde(rename = "retro.started")]
    RetroStarted(RetroStartedProps),
    #[serde(rename = "retro.completed")]
    RetroCompleted(RetroCompletedProps),
    #[serde(rename = "retro.failed")]
    RetroFailed(RetroFailedProps),
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
    actor:              Option<ActorRef>,
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
    actor:              Option<ActorRef>,
    event:              &'a str,
    properties:         &'a Value,
}

impl EventBody {
    pub fn event_name(&self) -> &str {
        match self {
            Self::RunCreated(_) => "run.created",
            Self::RunStarted(_) => "run.started",
            Self::RunSubmitted(_) => "run.submitted",
            Self::RunQueued(_) => "run.queued",
            Self::RunStarting(_) => "run.starting",
            Self::RunRunning(_) => "run.running",
            Self::RunBlocked(_) => "run.blocked",
            Self::RunUnblocked(_) => "run.unblocked",
            Self::RunRemoving(_) => "run.removing",
            Self::RunCancelRequested(_) => "run.cancel.requested",
            Self::RunPauseRequested(_) => "run.pause.requested",
            Self::RunUnpauseRequested(_) => "run.unpause.requested",
            Self::RunPaused(_) => "run.paused",
            Self::RunUnpaused(_) => "run.unpaused",
            Self::RunRewound(_) => "run.rewound",
            Self::RunArchived(_) => "run.archived",
            Self::RunUnarchived(_) => "run.unarchived",
            Self::RunCompleted(_) => "run.completed",
            Self::RunFailed(_) => "run.failed",
            Self::RunNotice(_) => "run.notice",
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
            Self::AgentCompactionStarted(_) => "agent.compaction.started",
            Self::AgentCompactionCompleted(_) => "agent.compaction.completed",
            Self::AgentLlmRetry(_) => "agent.llm.retry",
            Self::AgentSubSpawned(_) => "agent.sub.spawned",
            Self::AgentSubCompleted(_) => "agent.sub.completed",
            Self::AgentSubFailed(_) => "agent.sub.failed",
            Self::AgentSubClosed(_) => "agent.sub.closed",
            Self::AgentMcpReady(_) => "agent.mcp.ready",
            Self::AgentMcpFailed(_) => "agent.mcp.failed",
            Self::SubgraphStarted(_) => "subgraph.started",
            Self::SubgraphCompleted(_) => "subgraph.completed",
            Self::SandboxInitializing(_) => "sandbox.initializing",
            Self::SandboxReady(_) => "sandbox.ready",
            Self::SandboxFailed(_) => "sandbox.failed",
            Self::SandboxCleanupStarted(_) => "sandbox.cleanup.started",
            Self::SandboxCleanupCompleted(_) => "sandbox.cleanup.completed",
            Self::SandboxCleanupFailed(_) => "sandbox.cleanup.failed",
            Self::SnapshotPulling(_) => "sandbox.snapshot.pulling",
            Self::SnapshotPulled(_) => "sandbox.snapshot.pulled",
            Self::SnapshotEnsuring(_) => "sandbox.snapshot.ensuring",
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
            Self::AgentCliStarted(_) => "agent.cli.started",
            Self::AgentCliCompleted(_) => "agent.cli.completed",
            Self::PullRequestCreated(_) => "pull_request.created",
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
            Self::RetroStarted(_) => "retro.started",
            Self::RetroCompleted(_) => "retro.completed",
            Self::RetroFailed(_) => "retro.failed",
            Self::Unknown { name, .. } => name.as_str(),
        }
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
            | "run.queued"
            | "run.starting"
            | "run.running"
            | "run.blocked"
            | "run.unblocked"
            | "run.removing"
            | "run.rewound"
            | "run.archived"
            | "run.unarchived"
            | "run.completed"
            | "run.failed"
            | "run.notice"
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
            | "agent.compaction.started"
            | "agent.compaction.completed"
            | "agent.llm.retry"
            | "agent.sub.spawned"
            | "agent.sub.completed"
            | "agent.sub.failed"
            | "agent.sub.closed"
            | "agent.mcp.ready"
            | "agent.mcp.failed"
            | "subgraph.started"
            | "subgraph.completed"
            | "sandbox.initializing"
            | "sandbox.ready"
            | "sandbox.failed"
            | "sandbox.cleanup.started"
            | "sandbox.cleanup.completed"
            | "sandbox.cleanup.failed"
            | "sandbox.snapshot.pulling"
            | "sandbox.snapshot.pulled"
            | "sandbox.snapshot.ensuring"
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
            | "agent.cli.started"
            | "agent.cli.completed"
            | "pull_request.created"
            | "pull_request.failed"
            | "devcontainer.resolved"
            | "devcontainer.lifecycle.started"
            | "devcontainer.lifecycle.command.started"
            | "devcontainer.lifecycle.command.completed"
            | "devcontainer.lifecycle.completed"
            | "devcontainer.lifecycle.failed"
            | "retro.started"
            | "retro.completed"
            | "retro.failed"
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
    use crate::settings::SettingsLayer;
    use crate::{Edge, Graph, Node, RunBlobId, fixtures};

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
                duration_ms: 1234,
                status: crate::StageStatus::Success,
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
        let settings = SettingsLayer::default();
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
                "working_directory": "/tmp/run"
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
                "settings": SettingsLayer::default(),
                "graph": Graph::new("test"),
                "labels": {},
                "run_dir": "/tmp/run",
                "working_directory": "/tmp/run",
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
            duration_ms: 1234,
            status: crate::StageStatus::Success,
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
                "id": "ses_child",
                "display": "claude-sonnet"
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
        assert_eq!(actor.kind, ActorKind::Agent);
        assert_eq!(actor.id.as_deref(), Some("ses_child"));
        assert_eq!(actor.display.as_deref(), Some("claude-sonnet"));

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
        for event in ["run.queued", "run.blocked", "run.unblocked"] {
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
    fn run_archived_serializes_with_dotted_event_name_and_actor_property() {
        let body = EventBody::RunArchived(RunArchivedProps {
            actor: Some(ActorRef::user("alice".to_string())),
        });
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["event"], "run.archived");
        assert_eq!(value["properties"]["actor"]["kind"], "user");
        assert_eq!(value["properties"]["actor"]["id"], "alice");
    }

    #[test]
    fn run_unarchived_serializes_with_actor_only() {
        let body = EventBody::RunUnarchived(RunUnarchivedProps { actor: None });
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
            "properties": {
                "actor": {
                    "kind": "user",
                    "id": "alice",
                    "display": "alice"
                }
            }
        });

        let parsed = RunEvent::from_value(value.clone()).unwrap();
        assert!(matches!(parsed.body, EventBody::RunArchived(_)));
        let serialized = parsed.to_value().unwrap();
        assert_eq!(serialized["event"], "run.archived");
        assert_eq!(
            serialized["properties"]["actor"],
            value["properties"]["actor"]
        );
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
            EventBody::RunUnarchived(props) => {
                assert!(props.actor.is_none());
            }
            other => panic!("expected RunUnarchived body, got {other:?}"),
        }
    }

    #[test]
    fn run_queued_and_unblocked_round_trip_as_typed_events() {
        for value in [
            json!({
                "id": "evt_run_queued",
                "ts": "2026-04-19T12:00:00.000Z",
                "run_id": fixtures::RUN_1,
                "event": "run.queued",
                "properties": {}
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
}
