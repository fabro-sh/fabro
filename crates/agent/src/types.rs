use std::time::SystemTime;
use llm::types::{ContentPart, ToolCall, ToolResult, Usage};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    SessionStarted,
    SessionEnded,
    UserInput {
        text: String,
    },
    AssistantTextStart,
    AssistantMessage {
        text: String,
        model: String,
        usage: Usage,
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
    TurnLimitReached {
        max_turns: usize,
    },
    SkillExpanded {
        skill_name: String,
    },
    SteeringInjected {
        text: String,
    },
    CompactionStarted {
        estimated_tokens: usize,
        context_window_size: usize,
    },
    CompactionCompleted {
        original_turn_count: usize,
        preserved_turn_count: usize,
        summary_token_estimate: usize,
    },
    LlmRetry {
        provider: String,
        model: String,
        attempt: usize,
        delay_secs: f64,
        error: String,
    },
    SubAgentSpawned {
        agent_id: String,
        depth: usize,
        task: String,
    },
    SubAgentCompleted {
        agent_id: String,
        depth: usize,
        success: bool,
        turns_used: usize,
    },
    SubAgentFailed {
        agent_id: String,
        depth: usize,
        error: String,
    },
    SubAgentClosed {
        agent_id: String,
        depth: usize,
    },
    SubAgentEvent {
        agent_id: String,
        depth: usize,
        event: Box<AgentEvent>,
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
    fn subagent_spawned_constructible() {
        let event = AgentEvent::SubAgentSpawned {
            agent_id: "sa-1".into(),
            depth: 1,
            task: "list files".into(),
        };
        assert!(matches!(event, AgentEvent::SubAgentSpawned { depth: 1, .. }));
    }

    #[test]
    fn subagent_completed_constructible() {
        let event = AgentEvent::SubAgentCompleted {
            agent_id: "sa-1".into(),
            depth: 1,
            success: true,
            turns_used: 5,
        };
        assert!(matches!(event, AgentEvent::SubAgentCompleted { success: true, turns_used: 5, .. }));
    }

    #[test]
    fn subagent_failed_constructible() {
        let event = AgentEvent::SubAgentFailed {
            agent_id: "sa-1".into(),
            depth: 0,
            error: "timeout".into(),
        };
        assert!(matches!(event, AgentEvent::SubAgentFailed { depth: 0, .. }));
    }

    #[test]
    fn subagent_closed_constructible() {
        let event = AgentEvent::SubAgentClosed {
            agent_id: "sa-1".into(),
            depth: 2,
        };
        assert!(matches!(event, AgentEvent::SubAgentClosed { depth: 2, .. }));
    }

    #[test]
    fn subagent_event_wraps_child_event() {
        let child = AgentEvent::ToolCallStarted {
            tool_name: "read_file".into(),
            tool_call_id: "tc-1".into(),
            arguments: serde_json::json!({}),
        };
        let event = AgentEvent::SubAgentEvent {
            agent_id: "sa-1".into(),
            depth: 1,
            event: Box::new(child),
        };
        assert!(matches!(event, AgentEvent::SubAgentEvent { depth: 1, .. }));
    }

    #[test]
    fn subagent_events_serde_round_trip() {
        let events = vec![
            AgentEvent::SubAgentSpawned { agent_id: "sa-1".into(), depth: 0, task: "test".into() },
            AgentEvent::SubAgentCompleted { agent_id: "sa-1".into(), depth: 0, success: true, turns_used: 3 },
            AgentEvent::SubAgentFailed { agent_id: "sa-1".into(), depth: 0, error: "oops".into() },
            AgentEvent::SubAgentClosed { agent_id: "sa-1".into(), depth: 0 },
            AgentEvent::SubAgentEvent {
                agent_id: "sa-1".into(),
                depth: 1,
                event: Box::new(AgentEvent::SessionStarted),
            },
        ];
        let json = serde_json::to_string(&events).unwrap();
        let deserialized: Vec<AgentEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.len(), 5);
        assert!(matches!(&deserialized[4], AgentEvent::SubAgentEvent { event, .. } if matches!(event.as_ref(), AgentEvent::SessionStarted)));
    }

    #[test]
    fn agent_event_assistant_message() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            cache_read_tokens: Some(80),
            cache_write_tokens: Some(10),
            reasoning_tokens: Some(20),
            raw: None,
        };
        let event = AgentEvent::AssistantMessage {
            text: "Hello".into(),
            model: "test-model".into(),
            usage: usage.clone(),
            tool_call_count: 2,
        };
        match &event {
            AgentEvent::AssistantMessage { usage, tool_call_count, .. } => {
                assert_eq!(*tool_call_count, 2);
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.cache_read_tokens, Some(80));
                assert_eq!(usage.reasoning_tokens, Some(20));
            }
            _ => panic!("expected AssistantMessage"),
        }
    }
}
