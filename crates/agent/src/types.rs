use std::time::SystemTime;
use llm::types::{ContentPart, ToolCall, ToolResult, Usage};

#[derive(Debug, Clone)]
pub enum Turn {
    User {
        content: String,
        timestamp: SystemTime,
    },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
        reasoning: Option<String>,
        /// Opaque provider-specific content parts (e.g. OpenAI reasoning items)
        /// that must be preserved for round-tripping but don't map to standard fields.
        provider_parts: Vec<ContentPart>,
        usage: Usage,
        response_id: String,
        timestamp: SystemTime,
    },
    ToolResults {
        results: Vec<ToolResult>,
        timestamp: SystemTime,
    },
    /// Injected content sent as a system-role message to the LLM (maps to `Role::System`).
    System {
        content: String,
        timestamp: SystemTime,
    },
    /// Injected steering content sent as a user-role message to the LLM (maps to `Role::User`).
    /// Used to guide the assistant's behavior mid-conversation without appearing as actual user input.
    Steering {
        content: String,
        timestamp: SystemTime,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Processing,
    AwaitingInput,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    SessionStart,
    SessionEnd,
    UserInput,
    AssistantTextStart,
    AssistantTextDelta,
    AssistantTextEnd,
    ToolCallStart,
    ToolCallOutputDelta,
    ToolCallEnd,
    SteeringInjected,
    TurnLimit,
    LoopDetection,
    ContextWindowWarning,
    Error,
}

#[derive(Debug, Clone)]
pub enum EventData {
    Empty,
    ToolCall {
        tool_name: String,
        tool_call_id: String,
        arguments: serde_json::Value,
    },
    ToolCallEnd {
        tool_name: String,
        tool_call_id: String,
        output: serde_json::Value,
        is_error: bool,
    },
    Error {
        error: String,
    },
    ContextWarning {
        estimated_tokens: usize,
        context_window_size: usize,
        usage_percent: usize,
    },
}

#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub kind: EventKind,
    pub timestamp: SystemTime,
    pub session_id: String,
    pub data: EventData,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_event_construction() {
        let event = SessionEvent {
            kind: EventKind::SessionStart,
            timestamp: SystemTime::now(),
            session_id: "sess_1".into(),
            data: EventData::Empty,
        };
        assert_eq!(event.kind, EventKind::SessionStart);
        assert_eq!(event.session_id, "sess_1");
        assert!(matches!(event.data, EventData::Empty));
    }
}
