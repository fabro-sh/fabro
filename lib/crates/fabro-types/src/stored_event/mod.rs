pub mod agent;
pub mod infra;
pub mod misc;
pub mod run;
pub mod stage;

use chrono::{DateTime, Utc};
use serde::de::Error as DeError;
use serde::ser::Error as SerError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::RunId;

pub use agent::*;
pub use infra::*;
pub use misc::*;
pub use run::*;
pub use stage::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunNoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub run_id: RunId,
    pub event: String,
    pub node_id: Option<String>,
    pub node_label: Option<String>,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub properties: Value,
    pub body: EventBody,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "event", content = "properties")]
pub enum EventBody {
    #[serde(rename = "run.created")]
    RunCreated(RunCreatedProps),
    #[serde(rename = "run.started")]
    RunStarted(RunStartedProps),
    #[serde(rename = "run.submitted")]
    RunSubmitted(RunStatusTransitionProps),
    #[serde(rename = "run.starting")]
    RunStarting(RunStatusTransitionProps),
    #[serde(rename = "run.running")]
    RunRunning(RunStatusTransitionProps),
    #[serde(rename = "run.removing")]
    RunRemoving(RunStatusTransitionProps),
    #[serde(rename = "run.rewound")]
    RunRewound(RunRewoundProps),
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
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
struct StoredEventRaw {
    id: String,
    ts: DateTime<Utc>,
    run_id: RunId,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    node_label: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    parent_session_id: Option<String>,
    event: String,
    #[serde(default = "default_properties")]
    properties: Value,
}

fn default_properties() -> Value {
    Value::Object(Map::new())
}

impl StoredEvent {
    pub fn from_value(value: Value) -> serde_json::Result<Self> {
        let raw: StoredEventRaw = serde_json::from_value(value)?;
        let body = serde_json::from_value(json!({
            "event": raw.event,
            "properties": raw.properties,
        }))?;
        Ok(Self {
            id: raw.id,
            ts: raw.ts,
            run_id: raw.run_id,
            event: event_name_from_body(&body),
            node_id: raw.node_id,
            node_label: raw.node_label,
            session_id: raw.session_id,
            parent_session_id: raw.parent_session_id,
            properties: raw.properties,
            body,
        })
    }

    pub fn from_json_str(line: &str) -> serde_json::Result<Self> {
        Self::from_value(serde_json::from_str(line)?)
    }

    pub fn to_value(&self) -> serde_json::Result<Value> {
        let mut map = Map::new();
        map.insert("id".to_string(), serde_json::to_value(&self.id)?);
        map.insert("ts".to_string(), serde_json::to_value(self.ts)?);
        map.insert("run_id".to_string(), serde_json::to_value(self.run_id)?);
        map.insert(
            "event".to_string(),
            Value::String(event_name_from_body(&self.body)),
        );
        if let Some(value) = &self.session_id {
            map.insert("session_id".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = &self.parent_session_id {
            map.insert(
                "parent_session_id".to_string(),
                Value::String(value.clone()),
            );
        }
        if let Some(value) = &self.node_id {
            map.insert("node_id".to_string(), Value::String(value.clone()));
        }
        if let Some(value) = &self.node_label {
            map.insert("node_label".to_string(), Value::String(value.clone()));
        }
        map.insert("properties".to_string(), properties_from_body(&self.body));
        Ok(Value::Object(map))
    }

    pub fn event_name(&self) -> String {
        self.event.clone()
    }

    pub fn properties(&self) -> Value {
        self.properties.clone()
    }

    pub fn refresh_cache(&mut self) {
        self.event = event_name_from_body(&self.body);
        self.properties = properties_from_body(&self.body);
    }
}

fn event_name_from_body(body: &EventBody) -> String {
    serde_json::to_value(body)
        .ok()
        .and_then(|value| {
            value
                .get("event")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn properties_from_body(body: &EventBody) -> Value {
    serde_json::to_value(body)
        .ok()
        .and_then(|value| value.get("properties").cloned())
        .unwrap_or_else(default_properties)
}

impl Serialize for StoredEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StoredEvent {
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

    use crate::{Edge, Graph, Node, Settings, fixtures};

    use super::*;

    #[test]
    fn stored_event_round_trips_json() {
        let event = StoredEvent {
            id: "evt_1".to_string(),
            ts: DateTime::parse_from_rfc3339("2026-04-04T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id: fixtures::RUN_1,
            event: "stage.completed".to_string(),
            node_id: Some("build".to_string()),
            node_label: Some("Build".to_string()),
            session_id: None,
            parent_session_id: None,
            properties: json!({
                "index": 1,
                "duration_ms": 1234,
                "status": "success",
                "suggested_next_ids": ["next"],
                "notes": "done",
                "files_touched": ["src/main.rs"],
                "attempt": 1,
                "max_attempts": 1
            }),
            body: EventBody::StageCompleted(StageCompletedProps {
                index: 1,
                duration_ms: 1234,
                status: crate::StageStatus::Success,
                preferred_label: None,
                suggested_next_ids: vec!["next".to_string()],
                usage: None,
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
        let parsed = StoredEvent::from_value(value).unwrap();

        assert_eq!(parsed, event);
    }

    #[test]
    fn stored_event_deserializes_adjacent_layout() {
        let settings = Settings::default();
        let graph = Graph {
            name: "test".to_string(),
            nodes: HashMap::from([(
                "start".to_string(),
                Node {
                    id: "start".to_string(),
                    attrs: HashMap::new(),
                    classes: Vec::new(),
                },
            )]),
            edges: vec![Edge {
                from: "start".to_string(),
                to: "done".to_string(),
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

        let parsed = StoredEvent::from_value(line).unwrap();
        assert!(matches!(parsed.body, EventBody::RunCreated(_)));
    }
}
