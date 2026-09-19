//! The agent-side shapes the projection and the API keep: a coding agent
//! event placed on a stage, the names Fabro gives pebble's events, a
//! session's activation and tools, and a stage's prompt.

use lithos_llm::types::{ReasoningEffort, Speed};
use pebble_coding_agent::events::{CodingAgentEvent, CodingEvent, ToolSummary};
use serde::{Deserialize, Serialize};

use crate::PermissionLevel;

/// One coding-agent event placed on a workflow stage.
///
/// `event` is pebble's envelope verbatim, flattened into the properties so a
/// reader sees `seq`, `stream_id`, `session_id`, `timestamp`, and the
/// externally tagged `event` payload exactly as pebble serializes them.
/// `(stream_id, seq)` is the idempotency key for deduplication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEventProps {
    /// The node whose stage produced the event.
    pub stage: String,
    /// The graph visit of that stage.
    pub visit: u32,
    #[serde(flatten)]
    pub event: CodingAgentEvent,
}

impl AgentEventProps {
    #[must_use]
    pub fn new(stage: impl Into<String>, visit: u32, event: CodingAgentEvent) -> Self {
        Self {
            stage: stage.into(),
            visit,
            event,
        }
    }

    /// The `agent.*` (or `todo.*`) run event name for this event.
    #[must_use]
    pub fn event_name(&self) -> &'static str {
        coding_event_name(&self.event.event)
    }

    /// What happened, without the envelope.
    #[must_use]
    pub fn coding_event(&self) -> &CodingEvent {
        &self.event.event
    }
}

/// The run event name fabro derives from a pebble event variant.
///
/// Consumers switch on these names; the mapping is append-only.
#[must_use]
pub fn coding_event_name(event: &CodingEvent) -> &'static str {
    match event {
        CodingEvent::SessionStarted { .. } => "agent.session.started",
        CodingEvent::SessionEnded => "agent.session.ended",
        CodingEvent::ProcessingEnd => "agent.processing.end",
        CodingEvent::UserInput { .. } => "agent.input",
        CodingEvent::LlmRequestStarted { .. } => "agent.llm.started",
        CodingEvent::LlmFirstOutput { .. } => "agent.llm.first_output",
        CodingEvent::AssistantOutputReplace { .. } => "agent.output.replace",
        CodingEvent::AssistantMessage { .. } => "agent.message",
        CodingEvent::TextDelta { .. } => "agent.text.delta",
        CodingEvent::ReasoningDelta { .. } => "agent.reasoning.delta",
        CodingEvent::ToolCallStarted { .. } => "agent.tool.started",
        CodingEvent::ToolCallOutputDelta { .. } => "agent.tool.output.delta",
        CodingEvent::ToolCallCompleted { .. } => "agent.tool.completed",
        CodingEvent::ToolProcessCompleted { .. } => "agent.tool.process.completed",
        CodingEvent::Error { .. } => "agent.error",
        CodingEvent::Warning { .. } => "agent.warning",
        CodingEvent::LoopDetected => "agent.loop.detected",
        CodingEvent::ToolRoundsExhausted { .. } => "agent.tool.rounds.exhausted",
        CodingEvent::RouteFailover { .. } => "agent.route.failover",
        CodingEvent::RouteFailoverStopped { .. } => "agent.route.failover.stopped",
        CodingEvent::McpServerReady { .. } => "agent.mcp.server.ready",
        CodingEvent::McpServerFailed { .. } => "agent.mcp.server.failed",
        CodingEvent::McpServerDisconnected { .. } => "agent.mcp.server.disconnected",
        CodingEvent::SteeringInjected { .. } => "agent.steering.injected",
        CodingEvent::RoundInterrupted { .. } => "agent.round.interrupted",
        CodingEvent::CompactionStarted { .. } => "agent.compaction.started",
        CodingEvent::CompactionCompleted { .. } => "agent.compaction.completed",
        CodingEvent::CompactionFailed { .. } => "agent.compaction.failed",
        CodingEvent::CompactionCancelled { .. } => "agent.compaction.cancelled",
        CodingEvent::LlmRetry { .. } => "agent.llm.retry",
        CodingEvent::SubAgentSpawned { .. } => "agent.sub.spawned",
        CodingEvent::SubAgentTurnStarted { .. } => "agent.sub.turn.started",
        CodingEvent::SubAgentCompleted { .. } => "agent.sub.completed",
        CodingEvent::SubAgentFailed { .. } => "agent.sub.failed",
        CodingEvent::SubAgentClosed { .. } => "agent.sub.closed",
        CodingEvent::MemoryLoaded { .. } => "agent.memory.loaded",
        CodingEvent::SkillsDiscovered { .. } => "agent.skills.discovered",
        CodingEvent::SkillActivated { .. } => "agent.skill.activated",
        CodingEvent::TodoCreated(_) => "todo.created",
        CodingEvent::TodoUpdated(_) => "todo.updated",
        CodingEvent::TodoDeleted(_) => "todo.deleted",
        // `CodingEvent` is non-exhaustive: a variant this build does not know
        // still gets a stable, recognizable name instead of failing to store.
        _ => "agent.event",
    }
}

/// Every name [`coding_event_name`] can return.
pub const CODING_EVENT_NAMES: &[&str] = &[
    "agent.session.started",
    "agent.session.ended",
    "agent.processing.end",
    "agent.input",
    "agent.llm.started",
    "agent.llm.first_output",
    "agent.output.replace",
    "agent.message",
    "agent.text.delta",
    "agent.reasoning.delta",
    "agent.tool.started",
    "agent.tool.output.delta",
    "agent.tool.completed",
    "agent.tool.process.completed",
    "agent.error",
    "agent.warning",
    "agent.loop.detected",
    "agent.tool.rounds.exhausted",
    "agent.route.failover",
    "agent.route.failover.stopped",
    "agent.mcp.server.ready",
    "agent.mcp.server.failed",
    "agent.mcp.server.disconnected",
    "agent.steering.injected",
    "agent.round.interrupted",
    "agent.compaction.started",
    "agent.compaction.completed",
    "agent.compaction.failed",
    "agent.compaction.cancelled",
    "agent.llm.retry",
    "agent.sub.spawned",
    "agent.sub.turn.started",
    "agent.sub.completed",
    "agent.sub.failed",
    "agent.sub.closed",
    "agent.memory.loaded",
    "agent.skills.discovered",
    "agent.skill.activated",
    "todo.created",
    "todo.updated",
    "todo.deleted",
    "agent.event",
];

/// Whether `name` is a run event name derived from a pebble event.
#[must_use]
pub fn is_coding_event_name(name: &str) -> bool {
    CODING_EVENT_NAMES.contains(&name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCapability {
    Steer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionActivatedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed:            Option<Speed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_level: Option<PermissionLevel>,
    pub capabilities:     Vec<SessionCapability>,
    pub visit:            u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolsAvailableProps {
    #[serde(default)]
    pub tools: Vec<ToolSummary>,
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StagePromptProps {
    pub visit:            u32,
    pub text:             String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode:             Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed:            Option<Speed>,
}
