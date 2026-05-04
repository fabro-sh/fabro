use fabro_agent::{AgentEvent, SandboxEvent};

use super::Event;

#[must_use]
pub fn event_name(event: &Event) -> &'static str {
    match event {
        Event::RunCreated { .. } => "run.created",
        Event::WorkflowRunStarted { .. } => "run.started",
        Event::RunSubmitted { .. } => "run.submitted",
        Event::RunQueued => "run.queued",
        Event::RunStarting => "run.starting",
        Event::RunRunning => "run.running",
        Event::RunBlocked { .. } => "run.blocked",
        Event::RunUnblocked => "run.unblocked",
        Event::RunRemoving => "run.removing",
        Event::RunCancelRequested { .. } => "run.cancel.requested",
        Event::RunPauseRequested { .. } => "run.pause.requested",
        Event::RunUnpauseRequested { .. } => "run.unpause.requested",
        Event::RunPaused => "run.paused",
        Event::RunUnpaused => "run.unpaused",
        Event::RunSupersededBy { .. } => "run.superseded_by",
        Event::RunArchived { .. } => "run.archived",
        Event::RunUnarchived { .. } => "run.unarchived",
        Event::WorkflowRunCompleted { .. } => "run.completed",
        Event::WorkflowRunFailed { .. } => "run.failed",
        Event::RunNotice { .. } => "run.notice",
        Event::MetadataSnapshotStarted { .. } => "metadata.snapshot.started",
        Event::MetadataSnapshotCompleted { .. } => "metadata.snapshot.completed",
        Event::MetadataSnapshotFailed { .. } => "metadata.snapshot.failed",
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
        Event::SteeringAttached { .. } => "agent.steering.attached",
        Event::SteeringDetached { .. } => "agent.steering.detached",
        Event::SteerBuffered { .. } => "agent.steer.buffered",
        Event::SteerDropped { .. } => "agent.steer.dropped",
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

#[cfg(test)]
mod tests {
    use ::fabro_types::{ParallelBranchId, StageId};
    use fabro_agent::AgentEvent;

    use super::*;
    use crate::event::Event;

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
    fn run_archived_event_name_matches_dot_notation() {
        assert_eq!(
            event_name(&Event::RunArchived { actor: None }),
            "run.archived"
        );
        assert_eq!(
            event_name(&Event::RunUnarchived { actor: None }),
            "run.unarchived"
        );
    }
}