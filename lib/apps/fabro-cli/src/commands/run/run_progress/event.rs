use chrono::{DateTime, Utc};
use fabro_types::RunNoticeLevel;
use pebble_coding_agent::events::{CodingEvent, ErrorKind as AgentErrorKind, LlmOutputKind};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct ProgressUsage {
    pub(super) input_tokens:  u64,
    pub(super) output_tokens: u64,
    pub(super) cost:          Option<f64>,
}

impl ProgressUsage {
    pub(super) fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub(super) fn display_cost(&self) -> Option<f64> {
        self.cost
    }
}

#[derive(Debug, Clone)]
pub(super) enum ProgressEvent {
    RunCreated {
        web_url: Option<String>,
    },
    WorkflowStarted {
        worktree_dir: Option<String>,
        base_branch:  Option<String>,
        base_sha:     Option<String>,
    },
    StageStarted {
        node_id: String,
        name:    String,
        script:  Option<String>,
    },
    StageCompleted {
        node_id: String,
        name:    String,
        timing:  fabro_types::StageTiming,
        status:  String,
        usage:   Option<ProgressUsage>,
    },
    StageFailed {
        node_id: String,
        name:    String,
        error:   String,
    },
    StageRetrying {
        name:         String,
        attempt:      u64,
        max_attempts: u64,
        delay_ms:     u64,
    },
    ParallelStarted,
    ParallelBranchStarted {
        branch: String,
    },
    ParallelBranchCompleted {
        branch:      String,
        duration_ms: u64,
        status:      fabro_types::StageOutcome,
    },
    ParallelCompleted,
    AssistantMessage {
        stage_node_id: String,
        model:         String,
        root_session:  bool,
    },
    ToolCallStarted {
        stage_node_id: String,
        tool_name:     String,
        tool_call_id:  String,
        arguments:     Value,
        timestamp:     Option<DateTime<Utc>>,
    },
    ToolCallCompleted {
        stage_node_id: String,
        tool_call_id:  String,
        is_error:      bool,
        duration_ms:   Option<u64>,
        timestamp:     Option<DateTime<Utc>>,
    },
    ContextWindowWarning {
        stage_node_id: String,
        usage_percent: u64,
    },
    CompactionStarted {
        stage_node_id: String,
    },
    CompactionCompleted {
        stage_node_id:        String,
        original_turn_count:  u64,
        preserved_turn_count: u64,
        tracked_file_count:   u64,
    },
    CompactionFailed {
        stage_node_id: String,
        error:         String,
        root_session:  bool,
    },
    LlmRequestStarted {
        stage_node_id: String,
        model:         String,
    },
    LlmFirstOutput {
        stage_node_id: String,
        kind:          LlmOutputKind,
    },
    LlmRetry {
        stage_node_id: String,
        model:         String,
        attempt:       u64,
        delay_ms:      u64,
        error:         String,
    },
    LlmRequestFinished {
        stage_node_id: String,
    },
    /// A subagent started work. Generation 1 is the spawn; later generations
    /// are further turns in the same child session.
    SubagentStarted {
        stage_node_id: String,
        agent_id:      String,
        task:          String,
        generation:    u64,
    },
    SubagentCompleted {
        stage_node_id: String,
        agent_id:      String,
        success:       bool,
        turns_used:    u64,
    },
    EdgeSelected {
        from_node: String,
        to_node:   String,
        label:     Option<String>,
        condition: Option<String>,
    },
    LoopRestart {
        from_node: String,
        to_node:   String,
    },
    RunNotice {
        level:   RunNoticeLevel,
        code:    String,
        message: String,
    },
    PullRequestCreated {
        pr_url: String,
        draft:  bool,
    },
}

/// The progress line for one coding agent event, given whether it came
/// from the root session and when it was recorded: the mapping the legacy
/// envelope and a Petri stream envelope share.
pub(super) fn coding_progress_event(
    node_id: String,
    root_session: bool,
    timestamp: Option<DateTime<Utc>>,
    event: &CodingEvent,
) -> Option<ProgressEvent> {
    match event {
        CodingEvent::AssistantMessage { model, .. } => Some(ProgressEvent::AssistantMessage {
            stage_node_id: node_id,
            model: model.clone(),
            root_session,
        }),
        CodingEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } => Some(ProgressEvent::ToolCallStarted {
            stage_node_id: node_id,
            tool_name: tool_name.clone(),
            tool_call_id: tool_call_id.clone(),
            arguments: arguments.clone(),
            timestamp,
        }),
        CodingEvent::ToolCallCompleted {
            tool_call_id,
            is_error,
            ..
        } => Some(ProgressEvent::ToolCallCompleted {
            stage_node_id: node_id,
            tool_call_id: tool_call_id.clone(),
            is_error: *is_error,
            duration_ms: None,
            timestamp,
        }),
        CodingEvent::Warning { kind, details, .. } if kind == "context_window" => {
            let usage_percent = details
                .as_object()
                .and_then(|details| details.get("usage_percent"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            Some(ProgressEvent::ContextWindowWarning {
                stage_node_id: node_id,
                usage_percent,
            })
        }
        CodingEvent::CompactionStarted { .. } => Some(ProgressEvent::CompactionStarted {
            stage_node_id: node_id,
        }),
        CodingEvent::CompactionCompleted {
            original_turn_count,
            preserved_turn_count,
            tracked_file_count,
            ..
        } => Some(ProgressEvent::CompactionCompleted {
            stage_node_id:        node_id,
            original_turn_count:  *original_turn_count as u64,
            preserved_turn_count: *preserved_turn_count as u64,
            tracked_file_count:   *tracked_file_count as u64,
        }),
        CodingEvent::CompactionFailed { error, .. } => Some(ProgressEvent::CompactionFailed {
            stage_node_id: node_id,
            error: error.message.clone(),
            root_session,
        }),
        CodingEvent::Error { error } if error.kind == AgentErrorKind::Compaction => {
            Some(ProgressEvent::CompactionFailed {
                stage_node_id: node_id,
                error: error.message.clone(),
                root_session,
            })
        }
        CodingEvent::Error { .. } if root_session => Some(ProgressEvent::LlmRequestFinished {
            stage_node_id: node_id,
        }),
        CodingEvent::LlmRequestStarted { requested_model } if root_session => {
            Some(ProgressEvent::LlmRequestStarted {
                stage_node_id: node_id,
                model:         requested_model.clone(),
            })
        }
        CodingEvent::LlmFirstOutput { kind } if root_session => {
            Some(ProgressEvent::LlmFirstOutput {
                stage_node_id: node_id,
                kind:          *kind,
            })
        }
        CodingEvent::LlmRetry {
            model,
            attempt,
            delay_secs,
            error,
            ..
        } if root_session => {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "Retry delays are represented as small non-negative millisecond values."
            )]
            let delay_ms = (delay_secs * 1000.0) as u64;
            Some(ProgressEvent::LlmRetry {
                stage_node_id: node_id,
                model: model.clone(),
                attempt: *attempt as u64,
                delay_ms,
                error: error.message.clone(),
            })
        }
        CodingEvent::RoundInterrupted { .. } if root_session => {
            Some(ProgressEvent::LlmRequestFinished {
                stage_node_id: node_id,
            })
        }
        CodingEvent::SubAgentSpawned {
            agent_id,
            task,
            generation,
            ..
        }
        | CodingEvent::SubAgentTurnStarted {
            agent_id,
            task,
            generation,
            ..
        } => Some(ProgressEvent::SubagentStarted {
            stage_node_id: node_id,
            agent_id:      agent_id.clone(),
            task:          task.clone(),
            generation:    *generation,
        }),
        CodingEvent::SubAgentCompleted {
            agent_id,
            success,
            turns_used,
            ..
        } => Some(ProgressEvent::SubagentCompleted {
            stage_node_id: node_id,
            agent_id:      agent_id.clone(),
            success:       *success,
            turns_used:    *turns_used as u64,
        }),
        _ => None,
    }
}
