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
        /// Opaque provider-specific content parts (e.g. `OpenAI` reasoning items)
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

#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionStarted,
    SessionEnded,
    UserInput,
    AssistantTextStart,
    AssistantMessage {
        text: String,
        model: String,
        input_tokens: i64,
        output_tokens: i64,
        tool_call_count: usize,
    },
    TextDelta {
        delta: String,
    },
    ToolCallStarted {
        tool_name: String,
        tool_call_id: String,
        arguments: serde_json::Value,
    },
    ToolCallOutputDelta {
        delta: String,
    },
    ToolCallCompleted {
        tool_name: String,
        tool_call_id: String,
        output: serde_json::Value,
        is_error: bool,
    },
    Error {
        error: String,
    },
    ContextWindowWarning {
        estimated_tokens: usize,
        context_window_size: usize,
        usage_percent: usize,
    },
    LoopDetected,
    TurnLimitReached,
    SkillExpanded {
        skill_name: String,
    },
    SteeringInjected,
    CompactionStarted {
        estimated_tokens: usize,
        context_window_size: usize,
    },
    CompactionCompleted {
        original_turn_count: usize,
        preserved_turn_count: usize,
        summary_token_estimate: usize,
    },
}

#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub event: AgentEvent,
    pub timestamp: SystemTime,
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_event_construction() {
        let event = SessionEvent {
            event: AgentEvent::SessionStarted,
            timestamp: SystemTime::now(),
            session_id: "sess_1".into(),
        };
        assert!(matches!(event.event, AgentEvent::SessionStarted));
        assert_eq!(event.session_id, "sess_1");
    }

    #[test]
    fn compaction_events_constructible() {
        let started = AgentEvent::CompactionStarted {
            estimated_tokens: 5000,
            context_window_size: 8000,
        };
        assert!(matches!(started, AgentEvent::CompactionStarted { estimated_tokens: 5000, .. }));

        let completed = AgentEvent::CompactionCompleted {
            original_turn_count: 20,
            preserved_turn_count: 6,
            summary_token_estimate: 500,
        };
        assert!(matches!(completed, AgentEvent::CompactionCompleted { original_turn_count: 20, .. }));
    }

    #[test]
    fn skill_expanded_constructible() {
        let event = AgentEvent::SkillExpanded {
            skill_name: "commit".into(),
        };
        assert!(matches!(event, AgentEvent::SkillExpanded { skill_name } if skill_name == "commit"));
    }

    #[test]
    fn agent_event_assistant_message() {
        let event = AgentEvent::AssistantMessage {
            text: "Hello".into(),
            model: "test-model".into(),
            input_tokens: 100,
            output_tokens: 50,
            tool_call_count: 2,
        };
        assert!(matches!(event, AgentEvent::AssistantMessage { tool_call_count: 2, .. }));
    }
}
