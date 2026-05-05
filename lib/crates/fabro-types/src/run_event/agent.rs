use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::BilledTokenCounts;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionStartedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:    Option<String>,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionEndedProps {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCapability {
    Steer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionActivatedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:        Option<String>,
    pub capabilities: Vec<SessionCapability>,
    pub visit:        u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionDeactivatedProps {
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProcessingEndProps {
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInputProps {
    pub text:  String,
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessageProps {
    pub text:            String,
    pub model:           String,
    pub billing:         BilledTokenCounts,
    pub tool_call_count: usize,
    pub visit:           u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolStartedProps {
    pub tool_name:    String,
    pub tool_call_id: String,
    pub arguments:    Value,
    pub visit:        u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolCompletedProps {
    pub tool_name:    String,
    pub tool_call_id: String,
    pub output:       Value,
    pub is_error:     bool,
    pub visit:        u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentErrorProps {
    pub error: Value,
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentWarningProps {
    pub kind:    String,
    pub message: String,
    pub details: Value,
    pub visit:   u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLoopDetectedProps {
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTurnLimitReachedProps {
    pub max_turns: usize,
    pub visit:     u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSteeringInjectedProps {
    pub text:  String,
    pub visit: u32,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSteerBufferedProps {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSteerDroppedReason {
    QueueFull,
    RunEnded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSteerDroppedProps {
    pub reason: AgentSteerDroppedReason,
    pub count:  u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCompactionStartedProps {
    pub estimated_tokens:    usize,
    pub context_window_size: usize,
    pub visit:               u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCompactionCompletedProps {
    pub original_turn_count:    usize,
    pub preserved_turn_count:   usize,
    pub summary_token_estimate: usize,
    pub tracked_file_count:     usize,
    pub visit:                  u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLlmRetryProps {
    pub provider:   String,
    pub model:      String,
    pub attempt:    usize,
    pub delay_secs: f64,
    pub error:      Value,
    pub visit:      u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubSpawnedProps {
    pub agent_id: String,
    pub depth:    usize,
    pub task:     String,
    pub visit:    u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubCompletedProps {
    pub agent_id:   String,
    pub depth:      usize,
    pub success:    bool,
    pub turns_used: usize,
    pub visit:      u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubFailedProps {
    pub agent_id: String,
    pub depth:    usize,
    pub error:    Value,
    pub visit:    u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubClosedProps {
    pub agent_id: String,
    pub depth:    usize,
    pub visit:    u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMcpReadyProps {
    pub server_name: String,
    pub tool_count:  usize,
    pub visit:       u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMcpFailedProps {
    pub server_name: String,
    pub error:       String,
    pub visit:       u32,
}
