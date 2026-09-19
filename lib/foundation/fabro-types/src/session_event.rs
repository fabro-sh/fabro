//! The events of an Ask Fabro session.
//!
//! A session's conversation is pebble's session record; its events are the
//! live view of a turn: the turn starting, the user's message, the
//! assistant's deltas and messages, the tool calls, and the turn's end. They
//! are numbered per session, from 1, and stream to the session's clients as
//! they are recorded.

use chrono::{DateTime, Utc};
use lithos_llm::catalog::ProviderId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RunId, SessionId, TurnId};

/// One recorded event of a session.
///
/// On the wire the body is flattened: `event` names the kind and
/// `properties` holds its fields, beside `seq`, `session_id`, `run_id` and
/// `ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// The event's position in its session, from 1.
    pub seq:        u32,
    pub session_id: SessionId,
    pub run_id:     RunId,
    pub ts:         DateTime<Utc>,
    #[serde(flatten)]
    pub body:       SessionEventBody,
}

impl SessionEvent {
    #[must_use]
    pub fn event_name(&self) -> &'static str {
        self.body.event_name()
    }
}

/// What a session event records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "properties")]
pub enum SessionEventBody {
    #[serde(rename = "run.session.created")]
    Created(SessionCreatedProps),
    #[serde(rename = "run.session.turn.started")]
    TurnStarted(SessionTurnStartedProps),
    #[serde(rename = "run.session.user_message")]
    UserMessage(SessionUserMessageProps),
    #[serde(rename = "run.session.assistant_delta")]
    AssistantDelta(SessionAssistantDeltaProps),
    #[serde(rename = "run.session.assistant_message")]
    AssistantMessage(SessionAssistantMessageProps),
    #[serde(rename = "run.session.tool_call.started")]
    ToolCallStarted(SessionToolCallStartedProps),
    #[serde(rename = "run.session.tool_call.completed")]
    ToolCallCompleted(SessionToolCallCompletedProps),
    #[serde(rename = "run.session.turn.succeeded")]
    TurnSucceeded(SessionTurnSucceededProps),
    #[serde(rename = "run.session.turn.failed")]
    TurnFailed(SessionTurnFailedProps),
    #[serde(rename = "run.session.turn.interrupted")]
    TurnInterrupted(SessionTurnInterruptedProps),
}

impl SessionEventBody {
    #[must_use]
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::Created(_) => "run.session.created",
            Self::TurnStarted(_) => "run.session.turn.started",
            Self::UserMessage(_) => "run.session.user_message",
            Self::AssistantDelta(_) => "run.session.assistant_delta",
            Self::AssistantMessage(_) => "run.session.assistant_message",
            Self::ToolCallStarted(_) => "run.session.tool_call.started",
            Self::ToolCallCompleted(_) => "run.session.tool_call.completed",
            Self::TurnSucceeded(_) => "run.session.turn.succeeded",
            Self::TurnFailed(_) => "run.session.turn.failed",
            Self::TurnInterrupted(_) => "run.session.turn.interrupted",
        }
    }

    /// The turn the event belongs to; `None` for the session's creation.
    #[must_use]
    pub fn turn_id(&self) -> Option<TurnId> {
        match self {
            Self::Created(_) => None,
            Self::TurnStarted(props) => Some(props.turn_id),
            Self::UserMessage(props) => Some(props.turn_id),
            Self::AssistantDelta(props) => Some(props.turn_id),
            Self::AssistantMessage(props) => Some(props.turn_id),
            Self::ToolCallStarted(props) => Some(props.turn_id),
            Self::ToolCallCompleted(props) => Some(props.turn_id),
            Self::TurnSucceeded(props) => Some(props.turn_id),
            Self::TurnFailed(props) => Some(props.turn_id),
            Self::TurnInterrupted(props) => Some(props.turn_id),
        }
    }

    /// Whether the event ends a turn.
    #[must_use]
    pub fn ends_turn(&self) -> bool {
        matches!(
            self,
            Self::TurnSucceeded(_) | Self::TurnFailed(_) | Self::TurnInterrupted(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCreatedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTurnStartedProps {
    pub turn_id: TurnId,
    pub input:   String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUserMessageProps {
    pub turn_id: TurnId,
    pub text:    String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAssistantDeltaProps {
    pub turn_id: TurnId,
    pub delta:   String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAssistantMessageProps {
    pub turn_id: TurnId,
    pub text:    String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:   Option<String>,
    #[serde(default)]
    pub usage:   Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionToolCallStartedProps {
    pub turn_id:      TurnId,
    pub tool_name:    String,
    pub tool_call_id: String,
    pub arguments:    Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionToolCallCompletedProps {
    pub turn_id:               TurnId,
    pub tool_name:             String,
    pub tool_call_id:          String,
    pub output:                Value,
    pub is_error:              bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes_observed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes_retained: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes_omitted:  Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTurnSucceededProps {
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output:  Option<String>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SessionTurnFailedCode {
    NoSandbox,
    SandboxUnavailable,
    LlmUnconfigured,
    ModelUnavailable,
    ToolDenied,
    #[default]
    AgentError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTurnFailedProps {
    pub turn_id:   TurnId,
    pub error:     String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output:    Option<String>,
    #[serde(default)]
    pub code:      SessionTurnFailedCode,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTurnInterruptedProps {
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error:   Option<String>,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{
        SessionCreatedProps, SessionEvent, SessionEventBody, SessionToolCallCompletedProps,
        SessionTurnStartedProps,
    };
    use crate::{SessionId, TurnId, fixtures};

    #[test]
    fn a_session_event_flattens_its_body_on_the_wire() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let event = SessionEvent {
            seq: 2,
            session_id,
            run_id: fixtures::RUN_1,
            ts: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap(),
            body: SessionEventBody::TurnStarted(SessionTurnStartedProps {
                turn_id,
                input: "What happened?".to_string(),
            }),
        };

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            value,
            json!({
                "seq": 2,
                "session_id": session_id.to_string(),
                "run_id": fixtures::RUN_1,
                "ts": "2026-05-20T12:00:00Z",
                "event": "run.session.turn.started",
                "properties": { "turn_id": turn_id.to_string(), "input": "What happened?" }
            })
        );
        let round_trip: SessionEvent = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip, event);
        assert_eq!(round_trip.body.turn_id(), Some(turn_id));
        assert!(!round_trip.body.ends_turn());
    }

    #[test]
    fn a_created_event_has_no_turn() {
        let body = SessionEventBody::Created(SessionCreatedProps {
            title:    Some("Ask".to_string()),
            model:    None,
            provider: None,
        });
        assert_eq!(body.event_name(), "run.session.created");
        assert_eq!(body.turn_id(), None);
    }

    #[test]
    fn tool_completion_deserializes_without_output_byte_counts() {
        let props: SessionToolCallCompletedProps = serde_json::from_value(json!({
            "turn_id": TurnId::new(),
            "tool_name": "shell",
            "tool_call_id": "call_1",
            "output": "ok",
            "is_error": false
        }))
        .unwrap();

        assert!(props.output_bytes_observed.is_none());
        assert!(props.output_bytes_retained.is_none());
        assert!(props.output_bytes_omitted.is_none());
    }
}
