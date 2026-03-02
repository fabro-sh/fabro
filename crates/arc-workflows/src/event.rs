use std::sync::atomic::{AtomicI64, Ordering};

use serde::{Deserialize, Serialize};

use crate::outcome::StageUsage;
use arc_agent::{AgentEvent, ExecutionEnvEvent};

/// Events emitted during pipeline execution for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineEvent {
    PipelineStarted {
        name: String,
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_dir: Option<String>,
    },
    PipelineCompleted {
        duration_ms: u64,
        artifact_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_cost: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_git_commit_sha: Option<String>,
    },
    PipelineFailed {
        error: String,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_commit_sha: Option<String>,
    },
    StageStarted {
        name: String,
        index: usize,
        handler_type: Option<String>,
        attempt: usize,
        max_attempts: usize,
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
        attempt: usize,
        max_attempts: usize,
        failure_class: Option<String>,
    },
    StageFailed {
        name: String,
        index: usize,
        error: String,
        will_retry: bool,
        failure_reason: Option<String>,
        failure_class: Option<String>,
    },
    StageRetrying {
        name: String,
        index: usize,
        attempt: usize,
        max_attempts: usize,
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
    GitCheckpoint {
        run_id: String,
        node_id: String,
        status: String,
        git_commit_sha: String,
    },
    EdgeSelected {
        from_node: String,
        to_node: String,
        label: Option<String>,
        condition: Option<String>,
    },
    LoopRestart {
        from_node: String,
        to_node: String,
    },
    Prompt {
        stage: String,
        text: String,
    },
    /// Forwarded from an agent session, tagged with the pipeline stage.
    Agent {
        stage: String,
        event: AgentEvent,
    },
    ParallelEarlyTermination {
        reason: String,
        completed_count: usize,
        pending_count: usize,
    },
    SubgraphStarted {
        node_id: String,
        start_node: String,
    },
    SubgraphCompleted {
        node_id: String,
        steps_executed: usize,
        status: String,
        duration_ms: u64,
    },
    /// Forwarded from an execution environment lifecycle operation.
    ExecutionEnv {
        event: ExecutionEnvEvent,
    },
    SetupStarted {
        command_count: usize,
    },
    SetupCommandStarted {
        command: String,
        index: usize,
    },
    SetupCommandCompleted {
        command: String,
        index: usize,
        exit_code: i32,
        duration_ms: u64,
    },
    SetupCompleted {
        duration_ms: u64,
    },
    SetupFailed {
        command: String,
        index: usize,
        exit_code: i32,
        stderr: String,
    },
    StallWatchdogTimeout {
        node: String,
        idle_seconds: u64,
    },
}

impl PipelineEvent {
    pub fn trace(&self) {
        use tracing::{debug, error, info, warn};
        match self {
            Self::PipelineStarted { name, run_id, .. } => {
                info!(pipeline = name.as_str(), run_id, "Pipeline started");
            }
            Self::PipelineCompleted {
                duration_ms,
                artifact_count,
                ..
            } => {
                info!(duration_ms, artifact_count, "Pipeline completed");
            }
            Self::PipelineFailed {
                error, duration_ms, ..
            } => {
                error!(error, duration_ms, "Pipeline failed");
            }
            Self::StageStarted {
                name,
                index,
                handler_type,
                attempt,
                max_attempts,
            } => {
                debug!(
                    stage = name.as_str(),
                    index,
                    handler_type = handler_type.as_deref().unwrap_or(""),
                    attempt,
                    max_attempts,
                    "Stage started"
                );
            }
            Self::StageCompleted {
                name,
                index,
                duration_ms,
                status,
                attempt,
                max_attempts,
                ..
            } => {
                debug!(
                    stage = name.as_str(),
                    index,
                    duration_ms,
                    status,
                    attempt,
                    max_attempts,
                    "Stage completed"
                );
            }
            Self::StageFailed {
                name,
                index,
                error,
                will_retry,
                ..
            } => {
                if *will_retry {
                    warn!(
                        stage = name.as_str(),
                        index,
                        error,
                        will_retry,
                        "Stage failed"
                    );
                } else {
                    error!(
                        stage = name.as_str(),
                        index,
                        error,
                        will_retry,
                        "Stage failed"
                    );
                }
            }
            Self::StageRetrying {
                name,
                index,
                attempt,
                max_attempts,
                delay_ms,
            } => {
                warn!(
                    stage = name.as_str(),
                    index,
                    attempt,
                    max_attempts,
                    delay_ms,
                    "Stage retrying"
                );
            }
            Self::ParallelStarted {
                branch_count,
                join_policy,
                error_policy,
            } => {
                debug!(
                    branch_count,
                    join_policy,
                    error_policy,
                    "Parallel execution started"
                );
            }
            Self::ParallelBranchStarted { branch, index } => {
                debug!(branch, index, "Parallel branch started");
            }
            Self::ParallelBranchCompleted {
                branch,
                index,
                duration_ms,
                status,
            } => {
                debug!(
                    branch,
                    index,
                    duration_ms,
                    status,
                    "Parallel branch completed"
                );
            }
            Self::ParallelCompleted {
                duration_ms,
                success_count,
                failure_count,
            } => {
                debug!(
                    duration_ms,
                    success_count,
                    failure_count,
                    "Parallel execution completed"
                );
            }
            Self::InterviewStarted {
                stage,
                question_type,
                ..
            } => {
                debug!(stage, question_type, "Interview started");
            }
            Self::InterviewCompleted { duration_ms, .. } => {
                debug!(duration_ms, "Interview completed");
            }
            Self::InterviewTimeout {
                stage, duration_ms, ..
            } => {
                warn!(stage, duration_ms, "Interview timeout");
            }
            Self::CheckpointSaved { node_id } => {
                debug!(node_id, "Checkpoint saved");
            }
            Self::GitCheckpoint {
                run_id,
                node_id,
                status,
                ..
            } => {
                debug!(run_id, node_id, status, "Git checkpoint");
            }
            Self::EdgeSelected {
                from_node,
                to_node,
                label,
                ..
            } => {
                debug!(
                    from_node,
                    to_node,
                    label = label.as_deref().unwrap_or(""),
                    "Edge selected"
                );
            }
            Self::LoopRestart {
                from_node,
                to_node,
            } => {
                debug!(from_node, to_node, "Loop restart");
            }
            Self::Prompt { stage, text } => {
                debug!(stage, text_len = text.len(), "Prompt sent");
            }
            Self::Agent { .. } => {}
            Self::ExecutionEnv { .. } => {}
            Self::ParallelEarlyTermination {
                reason,
                completed_count,
                pending_count,
            } => {
                warn!(
                    reason,
                    completed_count,
                    pending_count,
                    "Parallel early termination"
                );
            }
            Self::SubgraphStarted {
                node_id,
                start_node,
            } => {
                debug!(node_id, start_node, "Subgraph started");
            }
            Self::SubgraphCompleted {
                node_id,
                steps_executed,
                status,
                duration_ms,
            } => {
                debug!(
                    node_id,
                    steps_executed,
                    status,
                    duration_ms,
                    "Subgraph completed"
                );
            }
            Self::SetupStarted { command_count } => {
                info!(command_count, "Setup started");
            }
            Self::SetupCommandStarted { command, index } => {
                debug!(command, index, "Setup command started");
            }
            Self::SetupCommandCompleted {
                command,
                index,
                exit_code,
                duration_ms,
            } => {
                debug!(
                    command,
                    index,
                    exit_code,
                    duration_ms,
                    "Setup command completed"
                );
            }
            Self::SetupCompleted { duration_ms } => {
                info!(duration_ms, "Setup completed");
            }
            Self::SetupFailed {
                command,
                index,
                exit_code,
                ..
            } => {
                error!(command, index, exit_code, "Setup command failed");
            }
            Self::StallWatchdogTimeout {
                node,
                idle_seconds,
            } => {
                warn!(node, idle_seconds, "Stall watchdog timeout");
            }
        }
    }
}

/// Current time as epoch milliseconds.
fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Listener callback type for pipeline events.
type EventListener = Box<dyn Fn(&PipelineEvent) + Send + Sync>;

/// Callback-based event emitter for pipeline events.
pub struct EventEmitter {
    listeners: Vec<EventListener>,
    /// Epoch milliseconds of the last `emit()` or `touch()` call. 0 until first event.
    last_event_at: AtomicI64,
}

impl std::fmt::Debug for EventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventEmitter")
            .field("listener_count", &self.listeners.len())
            .field("last_event_at", &self.last_event_at.load(Ordering::Relaxed))
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
            last_event_at: AtomicI64::new(0),
        }
    }

    pub fn on_event(&mut self, listener: impl Fn(&PipelineEvent) + Send + Sync + 'static) {
        self.listeners.push(Box::new(listener));
    }

    pub fn emit(&self, event: &PipelineEvent) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
        event.trace();
        for listener in &self.listeners {
            listener(event);
        }
    }

    /// Returns the epoch milliseconds of the last `emit()` or `touch()` call.
    /// Returns 0 if neither has been called.
    pub fn last_event_at(&self) -> i64 {
        self.last_event_at.load(Ordering::Relaxed)
    }

    /// Manually update the last-event timestamp (e.g. to seed the watchdog at pipeline start).
    pub fn touch(&self) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_llm::types::Usage;
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
            run_id: "1".to_string(),
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
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
            attempt: 1,
            max_attempts: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StageStarted"));
        assert!(json.contains("plan"));
        assert!(json.contains("\"handler_type\":\"codergen\""));
        assert!(json.contains("\"attempt\":1"));
        assert!(json.contains("\"max_attempts\":3"));

        // None handler_type serializes as null
        let event_none = PipelineEvent::StageStarted {
            name: "plan".to_string(),
            index: 0,
            handler_type: None,
            attempt: 1,
            max_attempts: 1,
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
    fn agent_event_wrapper_serialization() {
        let event = PipelineEvent::Agent {
            stage: "plan".to_string(),
            event: AgentEvent::ToolCallStarted {
                tool_name: "read_file".to_string(),
                tool_call_id: "call_1".to_string(),
                arguments: serde_json::json!({"path": "/tmp/test.txt"}),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Agent"));
        assert!(json.contains("ToolCallStarted"));
        assert!(json.contains("read_file"));
        assert!(json.contains("plan"));

        // Verify round-trip
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::Agent { stage, .. } if stage == "plan"));
    }

    #[test]
    fn agent_assistant_message_serialization() {
        let event = PipelineEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::AssistantMessage {
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
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("AssistantMessage"));
        assert!(json.contains("claude-opus-4-6"));
        assert!(json.contains("\"cache_read_tokens\":800"));
        assert!(json.contains("\"reasoning_tokens\":100"));

        // Round-trip
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            PipelineEvent::Agent {
                event: AgentEvent::AssistantMessage { usage, .. },
                ..
            } => {
                assert_eq!(usage.cache_read_tokens, Some(800));
                assert_eq!(usage.reasoning_tokens, Some(100));
            }
            _ => panic!("expected Agent(AssistantMessage)"),
        }
    }

    #[test]
    fn agent_assistant_message_without_cache_tokens_omits_them() {
        let event = PipelineEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::AssistantMessage {
                text: "response".to_string(),
                model: "test-model".to_string(),
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                    ..Default::default()
                },
                tool_call_count: 0,
            },
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
            attempt: 2,
            max_attempts: 3,
            failure_class: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"failure_reason\":\"lint errors remain\""));
        assert!(json.contains("\"notes\":\"fixed 3 of 5 issues\""));
        assert!(json.contains("src/main.rs"));
        assert!(json.contains("\"attempt\":2"));
        assert!(json.contains("\"max_attempts\":3"));
        assert!(json.contains("\"failure_class\":null"));

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
            attempt: 1,
            max_attempts: 1,
            failure_class: None,
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
            failure_class: Some("transient".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"failure_reason\":\"LLM request timed out\""));
        assert!(json.contains("\"failure_class\":\"transient\""));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, PipelineEvent::StageFailed { failure_class: Some(fc), .. } if fc == "transient")
        );

        let event_none = PipelineEvent::StageFailed {
            name: "plan".to_string(),
            index: 0,
            error: "timeout".to_string(),
            will_retry: false,
            failure_reason: None,
            failure_class: Some("terminal".to_string()),
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"failure_reason\":null"));
        assert!(json_none.contains("\"failure_class\":\"terminal\""));
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
        assert!(
            matches!(deserialized, PipelineEvent::ParallelBranchCompleted { status, .. } if status == "success")
        );
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
        assert!(
            matches!(deserialized, PipelineEvent::ParallelStarted { join_policy, error_policy, .. } if join_policy == "wait_all" && error_policy == "continue")
        );
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
        assert!(
            matches!(deserialized, PipelineEvent::InterviewStarted { question_type, .. } if question_type == "multiple_choice")
        );
    }

    #[test]
    fn agent_compaction_event_serialization() {
        let started = PipelineEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::CompactionStarted {
                estimated_tokens: 5000,
                context_window_size: 8000,
            },
        };
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains("CompactionStarted"));
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::Agent { stage, .. } if stage == "code"));

        let completed = PipelineEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::CompactionCompleted {
                original_turn_count: 20,
                preserved_turn_count: 6,
                summary_token_estimate: 500,
                tracked_file_count: 3,
            },
        };
        let json = serde_json::to_string(&completed).unwrap();
        assert!(json.contains("CompactionCompleted"));
        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::Agent { stage, .. } if stage == "code"));
    }

    #[test]
    fn edge_selected_event_serialization() {
        let event = PipelineEvent::EdgeSelected {
            from_node: "plan".to_string(),
            to_node: "code".to_string(),
            label: Some("success".to_string()),
            condition: Some("outcome == 'success'".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("EdgeSelected"));
        assert!(json.contains("\"from_node\":\"plan\""));
        assert!(json.contains("\"to_node\":\"code\""));
        assert!(json.contains("\"label\":\"success\""));
        assert!(json.contains("\"condition\":\"outcome == 'success'\""));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, PipelineEvent::EdgeSelected { from_node, to_node, .. } if from_node == "plan" && to_node == "code")
        );

        // None label/condition
        let event_none = PipelineEvent::EdgeSelected {
            from_node: "a".to_string(),
            to_node: "b".to_string(),
            label: None,
            condition: None,
        };
        let json_none = serde_json::to_string(&event_none).unwrap();
        assert!(json_none.contains("\"label\":null"));
        assert!(json_none.contains("\"condition\":null"));
    }

    #[test]
    fn loop_restart_event_serialization() {
        let event = PipelineEvent::LoopRestart {
            from_node: "review".to_string(),
            to_node: "code".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("LoopRestart"));
        assert!(json.contains("\"from_node\":\"review\""));
        assert!(json.contains("\"to_node\":\"code\""));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, PipelineEvent::LoopRestart { from_node, to_node } if from_node == "review" && to_node == "code")
        );
    }

    #[test]
    fn stage_retrying_event_serialization() {
        let event = PipelineEvent::StageRetrying {
            name: "lint".to_string(),
            index: 2,
            attempt: 3,
            max_attempts: 5,
            delay_ms: 400,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StageRetrying"));
        assert!(json.contains("\"attempt\":3"));
        assert!(json.contains("\"max_attempts\":5"));
        assert!(json.contains("\"delay_ms\":400"));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            PipelineEvent::StageRetrying {
                max_attempts: 5,
                ..
            }
        ));
    }

    #[test]
    fn agent_llm_retry_event_serialization() {
        let event = PipelineEvent::Agent {
            stage: "code".to_string(),
            event: AgentEvent::LlmRetry {
                provider: "anthropic".to_string(),
                model: "claude-opus-4-6".to_string(),
                attempt: 2,
                delay_secs: 1.5,
                error: "rate limited".to_string(),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("LlmRetry"));
        assert!(json.contains("\"provider\":\"anthropic\""));
        assert!(json.contains("\"delay_secs\":1.5"));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::Agent { stage, .. } if stage == "code"));
    }

    #[test]
    fn parallel_early_termination_event_serialization() {
        let event = PipelineEvent::ParallelEarlyTermination {
            reason: "fail_fast_branch_failed".to_string(),
            completed_count: 2,
            pending_count: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ParallelEarlyTermination"));
        assert!(json.contains("\"completed_count\":2"));
        assert!(json.contains("\"pending_count\":3"));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            PipelineEvent::ParallelEarlyTermination {
                completed_count: 2,
                ..
            }
        ));
    }

    #[test]
    fn subgraph_started_event_serialization() {
        let event = PipelineEvent::SubgraphStarted {
            node_id: "sub_1".to_string(),
            start_node: "start".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("SubgraphStarted"));
        assert!(json.contains("\"node_id\":\"sub_1\""));
        assert!(json.contains("\"start_node\":\"start\""));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, PipelineEvent::SubgraphStarted { node_id, .. } if node_id == "sub_1")
        );
    }

    #[test]
    fn subgraph_completed_event_serialization() {
        let event = PipelineEvent::SubgraphCompleted {
            node_id: "sub_1".to_string(),
            steps_executed: 5,
            status: "success".to_string(),
            duration_ms: 3200,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("SubgraphCompleted"));
        assert!(json.contains("\"steps_executed\":5"));
        assert!(json.contains("\"duration_ms\":3200"));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            PipelineEvent::SubgraphCompleted {
                steps_executed: 5,
                ..
            }
        ));
    }

    #[test]
    fn execution_env_event_wrapper_serialization() {
        use arc_agent::ExecutionEnvEvent;

        let event = PipelineEvent::ExecutionEnv {
            event: ExecutionEnvEvent::Initializing {
                env_type: "docker".into(),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ExecutionEnv"));
        assert!(json.contains("Initializing"));
        assert!(json.contains("docker"));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, PipelineEvent::ExecutionEnv { .. }));
    }

    #[test]
    fn emitter_last_event_at_initially_zero() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.last_event_at(), 0);
    }

    #[test]
    fn emitter_last_event_at_updates_after_emit() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.last_event_at(), 0);
        emitter.emit(&PipelineEvent::PipelineStarted {
            name: "test".to_string(),
            run_id: "1".to_string(),
            base_sha: None,
            run_branch: None,
            worktree_dir: None,
        });
        assert!(emitter.last_event_at() > 0);
    }

    #[test]
    fn emitter_touch_updates_last_event_at() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.last_event_at(), 0);
        emitter.touch();
        assert!(emitter.last_event_at() > 0);
    }

    #[test]
    fn stall_watchdog_timeout_serialization() {
        let event = PipelineEvent::StallWatchdogTimeout {
            node: "work".to_string(),
            idle_seconds: 600,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StallWatchdogTimeout"));
        assert!(json.contains("\"node\":\"work\""));
        assert!(json.contains("\"idle_seconds\":600"));

        let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(deserialized, PipelineEvent::StallWatchdogTimeout { node, idle_seconds } if node == "work" && idle_seconds == 600)
        );
    }

    #[test]
    fn setup_events_serialization() {
        let events = vec![
            PipelineEvent::SetupStarted { command_count: 3 },
            PipelineEvent::SetupCommandStarted {
                command: "npm install".into(),
                index: 0,
            },
            PipelineEvent::SetupCommandCompleted {
                command: "npm install".into(),
                index: 0,
                exit_code: 0,
                duration_ms: 5000,
            },
            PipelineEvent::SetupCompleted { duration_ms: 8000 },
            PipelineEvent::SetupFailed {
                command: "npm test".into(),
                index: 1,
                exit_code: 1,
                stderr: "test failed".into(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let deserialized: PipelineEvent = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }
}
