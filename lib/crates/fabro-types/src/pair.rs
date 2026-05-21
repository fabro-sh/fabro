use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr};

use crate::id::ulid_id;
use crate::{RunId, StageId};

ulid_id!(PairId);
ulid_id!(PairMessageId);

/// Maximum byte length of a pair message text payload.
///
/// Mirrored in `docs/public/api-reference/fabro-api.yaml` as the
/// `PairMessageRequest.text` `maxLength`. Keep the YAML and this constant in
/// sync.
pub const MAX_PAIR_MESSAGE_BYTES: usize = 8192;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PairStatus {
    Active,
    Ended,
    Failed,
}

impl PairStatus {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairTarget {
    pub stage_id:   StageId,
    pub node_label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairRecord {
    pub pair_id:        PairId,
    pub run_id:         RunId,
    pub status:         PairStatus,
    pub started_at:     DateTime<Utc>,
    pub ended_at:       Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
    pub target:         PairTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunPairStatusResponse {
    pub run_id:       RunId,
    pub current_pair: Option<PairRecord>,
    pub targets:      Vec<PairTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStartRequest {
    pub stage_id: StageId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairMessageRequest {
    pub text:              String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairMessageRecord {
    pub message_id:        PairMessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    pub pair_id:           PairId,
    pub run_id:            RunId,
    pub stage_id:          StageId,
    pub text:              String,
    pub accepted_at:       DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTranscriptResponse {
    pub data: Vec<PairTranscriptEntry>,
    pub meta: PairTranscriptMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairTranscriptMeta {
    pub next_since_seq: u32,
    pub has_more:       bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PairTranscriptEntry {
    #[serde(rename = "user_message")]
    UserMessage(PairTranscriptUserMessage),
    #[serde(rename = "system_message")]
    SystemMessage(PairTranscriptSystemMessage),
    #[serde(rename = "assistant_message")]
    AssistantMessage(PairTranscriptAssistantMessage),
    #[serde(rename = "tool_call")]
    ToolCall(PairTranscriptToolCall),
    #[serde(rename = "error")]
    Error(PairTranscriptError),
    #[serde(rename = "warning")]
    Warning(PairTranscriptWarning),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTranscriptUserMessage {
    pub seq:               u32,
    pub event_id:          String,
    pub ts:                DateTime<Utc>,
    pub pair_id:           PairId,
    pub target:            PairTarget,
    pub message_id:        PairMessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    pub text:              String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTranscriptSystemMessage {
    pub seq:                 u32,
    pub event_id:            String,
    pub ts:                  DateTime<Utc>,
    pub pair_id:             PairId,
    pub target:              PairTarget,
    pub system_message_kind: PairSystemMessageKind,
    pub text:                String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTranscriptAssistantMessage {
    pub seq:             u32,
    pub event_id:        String,
    pub ts:              DateTime<Utc>,
    pub pair_id:         PairId,
    pub target:          PairTarget,
    pub text:            String,
    pub tool_call_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTranscriptToolCall {
    pub seq:          u32,
    pub event_id:     String,
    pub ts:           DateTime<Utc>,
    pub pair_id:      PairId,
    pub target:       PairTarget,
    pub tool_call_id: String,
    pub tool_name:    String,
    pub status:       PairTranscriptToolStatus,
    pub summary:      String,
    pub is_error:     bool,
    pub truncated:    bool,
    pub detail_ref:   PairTranscriptDetailRef,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PairTranscriptToolStatus {
    Started,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairTranscriptDetailRef {
    pub seq:          u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTranscriptError {
    pub seq:        u32,
    pub event_id:   String,
    pub ts:         DateTime<Utc>,
    pub pair_id:    PairId,
    pub target:     PairTarget,
    pub message:    String,
    pub detail_ref: PairTranscriptDetailRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTranscriptWarning {
    pub seq:          u32,
    pub event_id:     String,
    pub ts:           DateTime<Utc>,
    pub pair_id:      PairId,
    pub target:       PairTarget,
    pub warning_kind: String,
    pub message:      String,
    pub detail_ref:   PairTranscriptDetailRef,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PairSystemMessageKind {
    HumanJoined,
    HumanLeft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEventDetailResponse {
    pub event:              RunEventDetailEnvelope,
    pub properties:         Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content:            Option<RunEventDetailContent>,
    pub truncated:          bool,
    pub redacted:           bool,
    pub max_content_length: usize,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RunEventDetailContentKind {
    Text,
    ToolOutput,
    ToolArguments,
    Error,
    Details,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEventDetailEnvelope {
    pub seq:          u32,
    pub id:           String,
    pub ts:           DateTime<Utc>,
    pub run_id:       RunId,
    pub event:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor:        Option<crate::Principal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_label:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_id:     Option<StageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEventDetailContent {
    pub kind:  RunEventDetailContentKind,
    pub value: String,
}
