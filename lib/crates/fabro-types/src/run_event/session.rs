use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TurnId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionCreatedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionTurnStartedProps {
    pub turn_id: TurnId,
    pub input:   String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionUserMessageProps {
    pub turn_id: TurnId,
    pub text:    String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionAssistantDeltaProps {
    pub turn_id: TurnId,
    pub delta:   String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionAssistantMessageProps {
    pub turn_id: TurnId,
    pub text:    String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:   Option<String>,
    #[serde(default)]
    pub usage:   Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionToolCallStartedProps {
    pub turn_id:      TurnId,
    pub tool_name:    String,
    pub tool_call_id: String,
    pub arguments:    Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionToolCallCompletedProps {
    pub turn_id:      TurnId,
    pub tool_name:    String,
    pub tool_call_id: String,
    pub output:       Value,
    pub is_error:     bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionTurnSucceededProps {
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output:  Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionTurnFailedProps {
    pub turn_id: TurnId,
    pub error:   String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output:  Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSessionTurnInterruptedProps {
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error:   Option<String>,
}
