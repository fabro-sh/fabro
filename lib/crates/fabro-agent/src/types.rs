use std::time::SystemTime;

use chrono::{DateTime, Utc};
use fabro_llm::Error as LlmError;
use fabro_llm::types::{ContentPart, ThinkingData, TokenCounts, ToolCall, ToolResult};
use fabro_model::ModelRef;
use fabro_types::{SessionMessage, StageContextWindowProjection};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::Error;

mod system_time_iso8601 {
    use std::time::SystemTime;

    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::de::Error as DeError;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let dt: DateTime<Utc> = (*time).into();
        serializer.serialize_str(&dt.to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let dt = DateTime::parse_from_rfc3339(&s).map_err(DeError::custom)?;
        Ok(dt.with_timezone(&Utc).into())
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    User {
        content:   String,
        timestamp: SystemTime,
    },
    Assistant {
        content:        String,
        tool_calls:     Vec<ToolCall>,
        /// Provider-specific content parts (e.g. `OpenAI` reasoning items,
        /// `Anthropic` thinking blocks with signatures) preserved for
        /// round-tripping. Reasoning/thinking text is stored here as
        /// `ContentPart::Thinking`.
        provider_parts: Vec<ContentPart>,
        usage:          Box<TokenCounts>,
        response_id:    String,
        timestamp:      SystemTime,
    },
    ToolResults {
        results:   Vec<ToolResult>,
        timestamp: SystemTime,
    },
    /// Injected content sent as a system-role message to the LLM (maps to
    /// `Role::System`).
    System {
        content:   String,
        timestamp: SystemTime,
    },
    /// Injected steering content sent as a user-role message to the LLM (maps
    /// to `Role::User`). Used to guide the assistant's behavior
    /// mid-conversation without appearing as actual user input.
    Steering {
        content:   String,
        timestamp: SystemTime,
    },
}

impl Message {
    /// Extract the first non-redacted thinking/reasoning text from an
    /// `Assistant` turn's `provider_parts`, if any.
    #[must_use]
    pub fn reasoning_text(&self) -> Option<&str> {
        let Self::Assistant { provider_parts, .. } = self else {
            return None;
        };
        provider_parts.iter().find_map(|p| match p {
            ContentPart::Thinking(ThinkingData {
                text,
                redacted: false,
                ..
            }) => Some(text.as_str()),
            _ => None,
        })
    }

    #[must_use]
    pub fn to_session_message(&self) -> SessionMessage {
        match self {
            Self::User { content, timestamp } => SessionMessage::User {
                content:   content.clone(),
                timestamp: system_time_to_utc(*timestamp),
            },
            Self::Assistant {
                content,
                tool_calls,
                provider_parts,
                usage,
                response_id,
                timestamp,
            } => SessionMessage::Assistant {
                content:        content.clone(),
                tool_calls:     values_or_empty(tool_calls),
                provider_parts: values_or_empty(provider_parts),
                usage:          value_or_null(&**usage),
                response_id:    response_id.clone(),
                timestamp:      system_time_to_utc(*timestamp),
            },
            Self::ToolResults { results, timestamp } => SessionMessage::ToolResults {
                results:   values_or_empty(results),
                timestamp: system_time_to_utc(*timestamp),
            },
            Self::System { content, timestamp } => SessionMessage::System {
                content:   content.clone(),
                timestamp: system_time_to_utc(*timestamp),
            },
            Self::Steering { content, timestamp } => SessionMessage::Steering {
                content:   content.clone(),
                timestamp: system_time_to_utc(*timestamp),
            },
        }
    }

    pub fn from_session_message(message: &SessionMessage) -> Result<Self, serde_json::Error> {
        Ok(match message {
            SessionMessage::User { content, timestamp } => Self::User {
                content:   content.clone(),
                timestamp: utc_to_system_time(*timestamp),
            },
            SessionMessage::Assistant {
                content,
                tool_calls,
                provider_parts,
                usage,
                response_id,
                timestamp,
            } => Self::Assistant {
                content:        content.clone(),
                tool_calls:     values_from_json(tool_calls)?,
                provider_parts: values_from_json(provider_parts)?,
                usage:          Box::new(serde_json::from_value(usage.clone())?),
                response_id:    response_id.clone(),
                timestamp:      utc_to_system_time(*timestamp),
            },
            SessionMessage::ToolResults { results, timestamp } => Self::ToolResults {
                results:   values_from_json(results)?,
                timestamp: utc_to_system_time(*timestamp),
            },
            SessionMessage::System { content, timestamp } => Self::System {
                content:   content.clone(),
                timestamp: utc_to_system_time(*timestamp),
            },
            SessionMessage::Steering { content, timestamp } => Self::Steering {
                content:   content.clone(),
                timestamp: utc_to_system_time(*timestamp),
            },
        })
    }
}

fn system_time_to_utc(timestamp: SystemTime) -> DateTime<Utc> {
    timestamp.into()
}

fn utc_to_system_time(timestamp: DateTime<Utc>) -> SystemTime {
    timestamp.into()
}

fn value_or_null<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

fn values_or_empty<T: Serialize>(values: &[T]) -> Vec<serde_json::Value> {
    values.iter().map(value_or_null).collect()
}

fn values_from_json<T: DeserializeOwned>(
    values: &[serde_json::Value],
) -> Result<Vec<T>, serde_json::Error> {
    values.iter().cloned().map(serde_json::from_value).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Thinking,
    Executing,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileSummary {
    pub path:         String,
    pub byte_count:   usize,
    pub loaded_bytes: usize,
    pub truncated:    bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name:        String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationSource {
    Slash,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolSummary {
    pub name:          String,
    pub original_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    SessionStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model:    Option<String>,
    },
    SessionEnded,
    ProcessingEnd,
    UserInput {
        text: String,
    },
    AssistantTextStart,
    /// Replaces the current in-progress assistant output buffers.
    AssistantOutputReplace {
        text:      String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    AssistantMessage {
        text:            String,
        model:           ModelRef,
        usage:           TokenCounts,
        tool_call_count: usize,
    },
    TextDelta {
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolCallStarted {
        tool_name:    String,
        tool_call_id: String,
        arguments:    serde_json::Value,
    },
    ToolCallOutputDelta {
        delta: String,
    },
    ToolCallCompleted {
        tool_name:    String,
        tool_call_id: String,
        output:       serde_json::Value,
        is_error:     bool,
    },
    Error {
        error: Error,
    },
    Warning {
        kind:    String,
        message: String,
        details: serde_json::Value,
    },
    LoopDetected,
    TurnLimitReached {
        max_turns: usize,
    },
    SteeringInjected {
        text:  String,
        /// Principal that authored the steer. Lifted to top-level
        /// `RunEvent.actor` by the workflow event-conversion layer; never
        /// serialized into event props.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<fabro_types::Principal>,
    },
    CompactionStarted {
        estimated_tokens:    usize,
        context_window_size: usize,
    },
    CompactionCompleted {
        original_turn_count:    usize,
        preserved_turn_count:   usize,
        summary_token_estimate: usize,
        tracked_file_count:     usize,
    },
    LlmRetry {
        provider:   String,
        model:      String,
        attempt:    usize,
        delay_secs: f64,
        error:      LlmError,
    },
    ContextWindowSnapshot(StageContextWindowProjection),
    SubAgentSpawned {
        agent_id: String,
        depth:    usize,
        task:     String,
    },
    SubAgentCompleted {
        agent_id:   String,
        depth:      usize,
        success:    bool,
        turns_used: usize,
    },
    SubAgentFailed {
        agent_id: String,
        depth:    usize,
        error:    Error,
    },
    SubAgentClosed {
        agent_id: String,
        depth:    usize,
    },
    McpServerReady {
        server_name: String,
        tool_count:  usize,
        tools:       Vec<McpToolSummary>,
    },
    McpServerFailed {
        server_name: String,
        error:       String,
    },
    MemoryLoaded {
        provider_profile:   String,
        files:              Vec<MemoryFileSummary>,
        total_loaded_bytes: usize,
        budget_bytes:       usize,
    },
    SkillsDiscovered {
        provider_profile: String,
        source_dirs:      Vec<String>,
        skills:           Vec<SkillSummary>,
    },
    SkillActivated {
        skill_name: String,
        source:     SkillActivationSource,
    },
    /// New todo / task was created. Carries the full row so the projection
    /// can be reconstructed from `todo.created` alone.
    TodoCreated(fabro_types::TodoCreatedProps),
    /// Existing todo was mutated. Field-by-field optional patches; `None`
    /// means "leave alone". `metadata_patch` keys with `null` values delete
    /// that key in the projection.
    TodoUpdated(fabro_types::TodoUpdatedProps),
    /// Todo was removed.
    TodoDeleted(fabro_types::TodoDeletedProps),
}

impl AgentEvent {
    /// Returns `true` for streaming-delta and UI-noise variants that are
    /// typically filtered out before forwarding to the workflow event stream.
    pub fn is_streaming_noise(&self) -> bool {
        matches!(
            self,
            Self::AssistantTextStart
                | Self::AssistantOutputReplace { .. }
                | Self::TextDelta { .. }
                | Self::ReasoningDelta { .. }
                | Self::ToolCallOutputDelta { .. }
        )
    }

    pub fn trace(&self, session_id: &str) {
        use tracing::{debug, error, info, warn};
        match self {
            Self::SessionStarted { provider, model } => {
                info!(
                    session_id,
                    provider = provider.as_deref().unwrap_or(""),
                    model = model.as_deref().unwrap_or(""),
                    "Agent session started"
                );
            }
            Self::SessionEnded => {
                info!(session_id, "Agent session ended");
            }
            Self::ProcessingEnd => {
                debug!(session_id, "Processing cycle finished, session idle");
            }
            Self::UserInput { text } => {
                debug!(session_id, text_len = text.len(), "User input received");
            }
            Self::AssistantTextStart => {
                debug!(session_id, "Assistant response started");
            }
            Self::AssistantMessage {
                model,
                usage,
                tool_call_count,
                ..
            } => {
                info!(
                    session_id,
                    provider = %model.provider,
                    model = model.model_id.as_str(),
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    tool_call_count,
                    "Assistant message"
                );
            }
            Self::TextDelta { .. }
            | Self::ReasoningDelta { .. }
            | Self::AssistantOutputReplace { .. }
            | Self::ToolCallOutputDelta { .. } => {}
            Self::ToolCallStarted {
                tool_name,
                tool_call_id,
                ..
            } => {
                info!(
                    session_id,
                    tool = tool_name.as_str(),
                    tool_call_id,
                    "Tool call started"
                );
            }
            Self::ToolCallCompleted {
                tool_name,
                tool_call_id,
                is_error,
                ..
            } => {
                info!(
                    session_id,
                    tool = tool_name.as_str(),
                    tool_call_id,
                    is_error,
                    "Tool call completed"
                );
            }
            Self::Error { error } => {
                error!(session_id, error = %error, "Agent error");
            }
            Self::Warning { kind, message, .. } => {
                warn!(
                    session_id,
                    kind = kind.as_str(),
                    message = message.as_str(),
                    "Warning"
                );
            }
            Self::LoopDetected => {
                warn!(session_id, "Loop detected");
            }
            Self::TurnLimitReached { max_turns } => {
                warn!(session_id, max_turns, "Message limit reached");
            }
            Self::SteeringInjected { text, .. } => {
                debug!(session_id, text_len = text.len(), "Steering injected");
            }
            Self::CompactionStarted {
                estimated_tokens,
                context_window_size,
            } => {
                info!(
                    session_id,
                    estimated_tokens, context_window_size, "Context compaction started"
                );
            }
            Self::CompactionCompleted {
                original_turn_count,
                preserved_turn_count,
                summary_token_estimate,
                tracked_file_count,
            } => {
                info!(
                    session_id,
                    original_turn_count,
                    preserved_turn_count,
                    summary_token_estimate,
                    tracked_file_count,
                    "Context compaction completed"
                );
            }
            Self::LlmRetry {
                provider,
                model,
                attempt,
                delay_secs,
                error,
            } => {
                warn!(
                    session_id,
                    provider,
                    model,
                    attempt,
                    delay_secs,
                    error = %error,
                    "LLM request failed, retrying"
                );
            }
            Self::ContextWindowSnapshot(snapshot) => {
                debug!(
                    session_id,
                    provider = snapshot.provider.as_str(),
                    model = snapshot.model.as_str(),
                    input_tokens = snapshot.input_tokens,
                    context_window_tokens = snapshot.context_window_tokens,
                    count_method = %snapshot.count_method,
                    "Context window snapshot"
                );
            }
            Self::SubAgentSpawned {
                agent_id,
                depth,
                task,
            } => {
                debug!(session_id, agent_id, depth, task, "Sub-agent spawned");
            }
            Self::SubAgentCompleted {
                agent_id,
                depth,
                success,
                turns_used,
            } => {
                debug!(
                    session_id,
                    agent_id, depth, success, turns_used, "Sub-agent completed"
                );
            }
            Self::SubAgentFailed {
                agent_id,
                depth,
                error,
            } => {
                warn!(
                    session_id,
                    agent_id,
                    depth,
                    error = %error,
                    "Sub-agent failed"
                );
            }
            Self::SubAgentClosed { agent_id, depth } => {
                debug!(session_id, agent_id, depth, "Sub-agent closed");
            }
            Self::McpServerReady {
                server_name,
                tool_count,
                tools,
            } => {
                info!(
                    session_id,
                    server = server_name.as_str(),
                    tool_count,
                    summary_count = tools.len(),
                    "MCP server ready"
                );
            }
            Self::MemoryLoaded {
                provider_profile,
                files,
                total_loaded_bytes,
                budget_bytes,
            } => {
                info!(
                    session_id,
                    provider_profile = provider_profile.as_str(),
                    file_count = files.len(),
                    total_loaded_bytes,
                    budget_bytes,
                    "Agent memory loaded"
                );
            }
            Self::SkillsDiscovered {
                provider_profile,
                source_dirs,
                skills,
            } => {
                info!(
                    session_id,
                    provider_profile = %provider_profile,
                    skill_count = skills.len(),
                    source_dir_count = source_dirs.len(),
                    "Agent skills discovered"
                );
            }
            Self::SkillActivated { skill_name, source } => {
                debug!(
                    session_id,
                    skill = skill_name.as_str(),
                    source = ?source,
                    "Agent skill activated"
                );
            }
            Self::McpServerFailed { server_name, error } => {
                error!(
                    session_id,
                    server = server_name.as_str(),
                    error,
                    "MCP server failed"
                );
            }
            Self::TodoCreated(p) => {
                debug!(
                    session_id,
                    list_id = p.list_id.as_str(),
                    todo_id = p.todo_id.as_str(),
                    "Todo created"
                );
            }
            Self::TodoUpdated(p) => {
                debug!(
                    session_id,
                    list_id = p.list_id.as_str(),
                    todo_id = p.todo_id.as_str(),
                    "Todo updated"
                );
            }
            Self::TodoDeleted(p) => {
                debug!(
                    session_id,
                    list_id = p.list_id.as_str(),
                    todo_id = p.todo_id.as_str(),
                    "Todo deleted"
                );
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub event:             AgentEvent,
    #[serde(with = "system_time_iso8601")]
    pub timestamp:         SystemTime,
    pub session_id:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id:      Option<String>,
}

#[cfg(test)]
mod tests {
    use fabro_model::ProviderId;

    use super::*;

    #[test]
    fn session_event_construction() {
        let event = SessionEvent {
            event:             AgentEvent::SessionStarted {
                provider: Some("anthropic".into()),
                model:    Some("claude-opus".into()),
            },
            timestamp:         SystemTime::now(),
            session_id:        "sess_1".into(),
            parent_session_id: None,
            tool_call_id:      None,
        };
        assert!(matches!(event.event, AgentEvent::SessionStarted {
            provider: Some(_),
            model:    Some(_),
        }));
        assert_eq!(event.session_id, "sess_1");
        assert_eq!(event.parent_session_id, None);
    }

    #[test]
    fn compaction_events_constructible() {
        let started = AgentEvent::CompactionStarted {
            estimated_tokens:    5000,
            context_window_size: 8000,
        };
        assert!(matches!(started, AgentEvent::CompactionStarted {
            estimated_tokens: 5000,
            ..
        }));

        let completed = AgentEvent::CompactionCompleted {
            original_turn_count:    20,
            preserved_turn_count:   6,
            summary_token_estimate: 500,
            tracked_file_count:     3,
        };
        assert!(matches!(completed, AgentEvent::CompactionCompleted {
            original_turn_count: 20,
            ..
        }));
    }

    #[test]
    fn subagent_spawned_constructible() {
        let event = AgentEvent::SubAgentSpawned {
            agent_id: "sa-1".into(),
            depth:    1,
            task:     "list files".into(),
        };
        assert!(matches!(event, AgentEvent::SubAgentSpawned {
            depth: 1,
            ..
        }));
    }

    #[test]
    fn subagent_completed_constructible() {
        let event = AgentEvent::SubAgentCompleted {
            agent_id:   "sa-1".into(),
            depth:      1,
            success:    true,
            turns_used: 5,
        };
        assert!(matches!(event, AgentEvent::SubAgentCompleted {
            success: true,
            turns_used: 5,
            ..
        }));
    }

    #[test]
    fn subagent_failed_constructible() {
        let event = AgentEvent::SubAgentFailed {
            agent_id: "sa-1".into(),
            depth:    0,
            error:    Error::ToolExecution("timeout".into()),
        };
        assert!(matches!(event, AgentEvent::SubAgentFailed { depth: 0, .. }));
    }

    #[test]
    fn subagent_closed_constructible() {
        let event = AgentEvent::SubAgentClosed {
            agent_id: "sa-1".into(),
            depth:    2,
        };
        assert!(matches!(event, AgentEvent::SubAgentClosed { depth: 2, .. }));
    }

    #[test]
    fn subagent_events_serde_round_trip() {
        let events = vec![
            AgentEvent::SubAgentSpawned {
                agent_id: "sa-1".into(),
                depth:    0,
                task:     "test".into(),
            },
            AgentEvent::SubAgentCompleted {
                agent_id:   "sa-1".into(),
                depth:      0,
                success:    true,
                turns_used: 3,
            },
            AgentEvent::SubAgentFailed {
                agent_id: "sa-1".into(),
                depth:    0,
                error:    Error::ToolExecution("oops".into()),
            },
            AgentEvent::SubAgentClosed {
                agent_id: "sa-1".into(),
                depth:    0,
            },
        ];
        let json = serde_json::to_string(&events).unwrap();
        let deserialized: Vec<AgentEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.len(), 4);
    }

    #[test]
    fn session_event_serde_round_trip_without_parent_session_id() {
        let event = SessionEvent {
            event:             AgentEvent::SessionStarted {
                provider: Some("anthropic".into()),
                model:    Some("claude-opus".into()),
            },
            timestamp:         SystemTime::now(),
            session_id:        "sess_42".into(),
            parent_session_id: None,
            tool_call_id:      None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("sess_42"));
        assert!(json.contains("SessionStarted"));
        assert!(!json.contains("parent_session_id"));
        assert!(json.contains('T'));
        assert!(json.contains('Z'));

        let deserialized: SessionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, "sess_42");
        assert_eq!(deserialized.parent_session_id, None);
        assert!(matches!(deserialized.event, AgentEvent::SessionStarted {
            provider: Some(_),
            model:    Some(_),
        }));
    }

    #[test]
    fn session_event_serde_round_trip_with_parent_session_id() {
        let event = SessionEvent {
            event:             AgentEvent::SessionStarted {
                provider: Some("openai".into()),
                model:    Some("gpt-5.4".into()),
            },
            timestamp:         SystemTime::now(),
            session_id:        "sess_child".into(),
            parent_session_id: Some("sess_parent".into()),
            tool_call_id:      None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("sess_child"));
        assert!(json.contains("sess_parent"));

        let deserialized: SessionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, "sess_child");
        assert_eq!(
            deserialized.parent_session_id.as_deref(),
            Some("sess_parent")
        );
    }

    #[test]
    fn mcp_server_ready_constructible() {
        let event = AgentEvent::McpServerReady {
            server_name: "filesystem".into(),
            tool_count:  0,
            tools:       Vec::new(),
        };
        assert!(matches!(
            event,
            AgentEvent::McpServerReady { server_name, .. } if server_name == "filesystem"
        ));
    }

    #[test]
    fn mcp_server_failed_constructible() {
        let event = AgentEvent::McpServerFailed {
            server_name: "broken".into(),
            error:       "connection refused".into(),
        };
        assert!(
            matches!(event, AgentEvent::McpServerFailed { server_name, .. } if server_name == "broken")
        );
    }

    #[test]
    fn mcp_events_serde_round_trip() {
        let events = vec![
            AgentEvent::McpServerReady {
                server_name: "fs".into(),
                tool_count:  0,
                tools:       Vec::new(),
            },
            AgentEvent::McpServerFailed {
                server_name: "bad".into(),
                error:       "timeout".into(),
            },
        ];
        let json = serde_json::to_string(&events).unwrap();
        let deserialized: Vec<AgentEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.len(), 2);
        assert!(matches!(
            &deserialized[0],
            AgentEvent::McpServerReady { server_name, .. } if server_name == "fs"
        ));
        assert!(matches!(
            &deserialized[1],
            AgentEvent::McpServerFailed { .. }
        ));
    }

    #[test]
    fn agent_event_assistant_message() {
        let usage = TokenCounts {
            input_tokens:       100,
            output_tokens:      50,
            cache_read_tokens:  80,
            cache_write_tokens: 10,
            reasoning_tokens:   20,
        };
        let event = AgentEvent::AssistantMessage {
            text:            "Hello".into(),
            model:           ModelRef {
                provider: ProviderId::openai(),
                model_id: "test-model".into(),
                speed:    None,
            },
            usage:           usage.clone(),
            tool_call_count: 2,
        };
        match &event {
            AgentEvent::AssistantMessage {
                usage,
                tool_call_count,
                ..
            } => {
                assert_eq!(*tool_call_count, 2);
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.cache_read_tokens, 80);
                assert_eq!(usage.reasoning_tokens, 20);
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn agent_event_assistant_output_replace_roundtrip() {
        let event = AgentEvent::AssistantOutputReplace {
            text:      "Hello again".into(),
            reasoning: Some("Retrying from scratch".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            AgentEvent::AssistantOutputReplace { text, reasoning } => {
                assert_eq!(text, "Hello again");
                assert_eq!(reasoning.as_deref(), Some("Retrying from scratch"));
            }
            _ => panic!("expected AssistantOutputReplace"),
        }
    }

    // --- Phase 4: Typed error event tests ---

    #[test]
    fn error_event_serde_roundtrip_with_agent_error() {
        let event = AgentEvent::Error {
            error: Error::Llm(LlmError::Network {
                message: "refused".into(),
                source:  None,
            }),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            AgentEvent::Error { error } => {
                assert!(error.to_string().contains("refused"));
            }
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn llm_retry_event_carries_sdk_error() {
        use fabro_llm::error::{ProviderErrorDetail, ProviderErrorKind};
        let event = AgentEvent::LlmRetry {
            provider:   "openai".into(),
            model:      "gpt-4".into(),
            attempt:    1,
            delay_secs: 2.0,
            error:      LlmError::Provider {
                kind:   ProviderErrorKind::RateLimit,
                detail: Box::new(ProviderErrorDetail {
                    message:     "too fast".into(),
                    provider:    "openai".into(),
                    status_code: Some(429),
                    error_code:  None,
                    retry_after: Some(2.0),
                    raw:         None,
                }),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            AgentEvent::LlmRetry { error, .. } => {
                assert!(error.retryable());
                assert_eq!(error.retry_after(), Some(2.0));
            }
            _ => panic!("expected LlmRetry variant"),
        }
    }

    #[test]
    fn subagent_failed_carries_agent_error() {
        let event = AgentEvent::SubAgentFailed {
            agent_id: "sa-1".into(),
            depth:    0,
            error:    Error::ToolExecution("cmd failed".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            AgentEvent::SubAgentFailed { error, .. } => {
                assert!(error.to_string().contains("cmd failed"));
            }
            _ => panic!("expected SubAgentFailed variant"),
        }
    }

    #[test]
    fn error_event_preserves_error_type_through_json() {
        let event = AgentEvent::Error {
            error: Error::ToolExecution("cmd failed".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // The error field should contain the Error's tagged type
        assert_eq!(v["Error"]["error"]["type"], "tool_execution");
    }

    #[test]
    fn mcp_server_failed_still_string() {
        let event = AgentEvent::McpServerFailed {
            server_name: "broken".into(),
            error:       "connection refused".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            AgentEvent::McpServerFailed { error, .. } => {
                assert_eq!(error, "connection refused");
            }
            _ => panic!("expected McpServerFailed variant"),
        }
    }
}
