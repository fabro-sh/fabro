use serde::{Deserialize, Serialize};

use crate::outcome::StageUsage;
use llm::types::Usage;

/// Events emitted during pipeline execution for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineEvent {
    PipelineStarted {
        name: String,
        id: String,
    },
    PipelineCompleted {
        duration_ms: u64,
        artifact_count: usize,
    },
    PipelineFailed {
        error: String,
        duration_ms: u64,
    },
    StageStarted {
        name: String,
        index: usize,
        handler_type: Option<String>,
    },
    StageCompleted {
        name: String,
        index: usize,
        duration_ms: u64,
        status: String,
        preferred_label: Option<String>,
        suggested_next_ids: Vec<String>,
        usage: Option<StageUsage>,
        failure_reason: Option<String>,
        notes: Option<String>,
        files_touched: Vec<String>,
    },
    StageFailed {
        name: String,
        index: usize,
        error: String,
        will_retry: bool,
        failure_reason: Option<String>,
    },
    StageRetrying {
        name: String,
        index: usize,
        attempt: usize,
        delay_ms: u64,
    },
    ParallelStarted {
        branch_count: usize,
        join_policy: String,
        error_policy: String,
    },
    ParallelBranchStarted {
        branch: String,
        index: usize,
    },
    ParallelBranchCompleted {
        branch: String,
        index: usize,
        duration_ms: u64,
        status: String,
    },
    ParallelCompleted {
        duration_ms: u64,
        success_count: usize,
        failure_count: usize,
    },
    InterviewStarted {
        question: String,
        stage: String,
        question_type: String,
    },
    InterviewCompleted {
        question: String,
        answer: String,
        duration_ms: u64,
    },
    InterviewTimeout {
        question: String,
        stage: String,
        duration_ms: u64,
    },
    CheckpointSaved {
        node_id: String,
    },
    Prompt {
        stage: String,
        text: String,
    },
    AssistantMessage {
        stage: String,
        text: String,
        model: String,
        usage: Usage,
        tool_call_count: usize,
    },
    ToolCallStarted {
        stage: String,
        tool_name: String,
        tool_call_id: String,
        arguments: serde_json::Value,
    },
    ToolCallCompleted {
        stage: String,
        tool_name: String,
        tool_call_id: String,
        output: serde_json::Value,
        is_error: bool,
    },
    SessionError {
        stage: String,
        error: String,
    },
    ContextWindowWarning {
        stage: String,
        estimated_tokens: usize,
        context_window_size: usize,
        usage_percent: usize,
    },
    LoopDetected {
        stage: String,
    },
    TurnLimitReached {
        stage: String,
    },
    CompactionStarted {
        stage: String,
        estimated_tokens: usize,
        context_window_size: usize,
    },
    CompactionCompleted {
        stage: String,
        original_turn_count: usize,
        preserved_turn_count: usize,
        summary_token_estimate: usize,
    },
}

/// Listener callback type for pipeline events.
type EventListener = Box<dyn Fn(&PipelineEvent) + Send + Sync>;

/// Callback-based event emitter for pipeline events.
pub struct EventEmitter {
    listeners: Vec<EventListener>,
}

impl std::fmt::Debug for EventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventEmitter")
            .field("listener_count", &self.listeners.len())
            .finish()
    }
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }

    pub fn on_event(&mut self, listener: impl Fn(&PipelineEvent) + Send + Sync + 'static) {
        self.listeners.push(Box::new(listener));
    }

    pub fn emit(&self, event: &PipelineEvent) {
        for listener in &self.listeners {
            listener(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn event_emitter_new_has_no_listeners() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.listeners.len(), 0);
    }

    #[test]
    fn event_emitter_calls_listener() {
        let mut emitter = EventEmitter::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        emitter.on_event(move |event| {
            let name = match event {
                PipelineEvent::PipelineStarted { name, .. } => name.clone(),
                _ => "other".to_string(),
            };
            received_clone.lock().unwrap().push(name);
        });
        emitter.emit(&PipelineEvent::PipelineStarted {
            name: "test".to_string(),
            id: "1".to_string(),
        });
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], "test");
    }

    #[test]
    fn pipeline_event_serialization() {
        let event = PipelineEvent::StageStarted {
            name: "plan".to_string(),
            index: 0,
            handler_type: Some("codergen".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StageStarted"));
        assert!(json.contains("plan"));
        assert!(json.contains("\"handler_type\":\"codergen\""));

        // None handler_type serializes as null
        let event_none = PipelineEvent::StageStarted {
            name: "plan".to_string(),
            index: 0,
            handler_type: None,
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"handler_type\":null"));
    }

    #[test]
    fn event_emitter_default() {
        let emitter = EventEmitter::default();
        assert_eq!(emitter.listeners.len(), 0);
    }

    #[test]
    fn llm_conversation_event_serialization() {
        let event = PipelineEvent::ToolCallStarted {
            stage: "plan".to_string(),
            tool_name: "read_file".to_string(),
            tool_call_id: "call_1".to_string(),
            arguments: serde_json::json!({"path": "/tmp/test.txt"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ToolCallStarted"));
        assert!(json.contains("read_file"));
        assert!(json.contains("plan"));

        // Verify round-trip
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::ToolCallStarted { stage, .. } if stage == "plan"));
    }

    #[test]
    fn assistant_message_event_serialization() {
        let event = PipelineEvent::AssistantMessage {
            stage: "code".to_string(),
            text: "Here is the implementation".to_string(),
            model: "claude-opus-4-6".to_string(),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 500,
                total_tokens: 1500,
                cache_read_tokens: Some(800),
                cache_write_tokens: Some(50),
                reasoning_tokens: Some(100),
                raw: None,
            },
            tool_call_count: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("AssistantMessage"));
        assert!(json.contains("claude-opus-4-6"));
        assert!(json.contains("\"cache_read_tokens\":800"));
        assert!(json.contains("\"reasoning_tokens\":100"));

        // Round-trip
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            PipelineEvent::AssistantMessage { usage, .. } => {
                assert_eq!(usage.cache_read_tokens, Some(800));
                assert_eq!(usage.reasoning_tokens, Some(100));
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn assistant_message_without_cache_tokens_omits_them() {
        let event = PipelineEvent::AssistantMessage {
            stage: "code".to_string(),
            text: "response".to_string(),
            model: "test-model".to_string(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                ..Default::default()
            },
            tool_call_count: 0,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("cache_read_tokens"));
        assert!(!json.contains("reasoning_tokens"));
    }

    #[test]
    fn stage_completed_event_serialization_with_new_fields() {
        let event = PipelineEvent::StageCompleted {
            name: "plan".to_string(),
            index: 0,
            duration_ms: 1500,
            status: "partial_success".to_string(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure_reason: Some("lint errors remain".to_string()),
            notes: Some("fixed 3 of 5 issues".to_string()),
            files_touched: vec!["src/main.rs".to_string()],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"failure_reason\":\"lint errors remain\""));
        assert!(json.contains("\"notes\":\"fixed 3 of 5 issues\""));
        assert!(json.contains("src/main.rs"));

        let event_none = PipelineEvent::StageCompleted {
            name: "plan".to_string(),
            index: 0,
            duration_ms: 1500,
            status: "success".to_string(),
            preferred_label: None,
            suggested_next_ids: vec![],
            usage: None,
            failure_reason: None,
            notes: None,
            files_touched: vec![],
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"failure_reason\":null"));
        assert!(json_none.contains("\"notes\":null"));
    }

    #[test]
    fn stage_failed_event_serialization() {
        let event = PipelineEvent::StageFailed {
            name: "plan".to_string(),
            index: 0,
            error: "timeout".to_string(),
            will_retry: true,
            failure_reason: Some("LLM request timed out".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"failure_reason\":\"LLM request timed out\""));

        let event_none = PipelineEvent::StageFailed {
            name: "plan".to_string(),
            index: 0,
            error: "timeout".to_string(),
            will_retry: false,
            failure_reason: None,
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"failure_reason\":null"));
    }

    #[test]
    fn parallel_branch_completed_event_serialization() {
        let event = PipelineEvent::ParallelBranchCompleted {
            branch: "branch_a".to_string(),
            index: 0,
            duration_ms: 1500,
            status: "success".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(!json.contains("\"success\":"));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::ParallelBranchCompleted { status, .. } if status == "success"));
    }

    #[test]
    fn parallel_started_event_serialization() {
        let event = PipelineEvent::ParallelStarted {
            branch_count: 3,
            join_policy: "wait_all".to_string(),
            error_policy: "continue".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"join_policy\":\"wait_all\""));
        assert!(json.contains("\"error_policy\":\"continue\""));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::ParallelStarted { join_policy, error_policy, .. } if join_policy == "wait_all" && error_policy == "continue"));
    }

    #[test]
    fn interview_started_event_serialization() {
        let event = PipelineEvent::InterviewStarted {
            question: "Review changes?".to_string(),
            stage: "gate".to_string(),
            question_type: "multiple_choice".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"question_type\":\"multiple_choice\""));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::InterviewStarted { question_type, .. } if question_type == "multiple_choice"));
    }

    #[test]
    fn compaction_pipeline_event_serialization() {
        let started = PipelineEvent::CompactionStarted {
            stage: "code".to_string(),
            estimated_tokens: 5000,
            context_window_size: 8000,
        };
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains("CompactionStarted"));
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::CompactionStarted { stage, .. } if stage == "code"));

        let completed = PipelineEvent::CompactionCompleted {
            stage: "code".to_string(),
            original_turn_count: 20,
            preserved_turn_count: 6,
            summary_token_estimate: 500,
        };
        let json = serde_json::to_string(&completed).unwrap();
        assert!(json.contains("CompactionCompleted"));
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::CompactionCompleted { stage, .. } if stage == "code"));
    }
}
