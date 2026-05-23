use fabro_model::{ReasoningEffort, Speed};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::BilledTokenCounts;
use crate::transcript::{ToolCall, ToolResult, TranscriptMessage};
use crate::{
    MessageId, ModelRef, PairId, PairMessageId, PairSystemMessageKind, PermissionLevel,
    StageContextWindowProjection, StageId, TurnId,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionStartedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:    Option<String>,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionEndedProps {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCapability {
    Steer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionActivatedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed:            Option<Speed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_level: Option<PermissionLevel>,
    pub capabilities:     Vec<SessionCapability>,
    pub visit:            u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionDeactivatedProps {
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProcessingEndProps {
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInputProps {
    pub text:  String,
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessageProps {
    // Narrow legacy fields retained for consumer compatibility.
    pub text:            String,
    pub model:           ModelRef,
    pub billing:         BilledTokenCounts,
    pub tool_call_count: usize,
    pub visit:           u32,
    /// Canonical replay-authoritative transcript message. Present on events
    /// emitted after the unified transcript migration; absent on legacy
    /// payloads so older events still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message:         Option<TranscriptMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolStartedProps {
    // Narrow legacy fields retained for consumer compatibility.
    pub tool_name:         String,
    pub tool_call_id:      String,
    pub arguments:         Value,
    pub visit:             u32,
    /// Canonical tool call payload. Carries `tool_type`, `raw_arguments`, and
    /// `provider_metadata` (e.g. Gemini `thought_signature`) so tool actions
    /// can be replayed against the originating provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call:         Option<ToolCall>,
    /// Turn that initiated this tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id:           Option<TurnId>,
    /// Agent message id that owns this tool call. Minted before tool
    /// execution so tool actions can be linked back to their parent agent
    /// response in the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolCompletedProps {
    // Narrow legacy fields retained for consumer compatibility.
    pub tool_name:    String,
    pub tool_call_id: String,
    pub output:       Value,
    pub is_error:     bool,
    pub visit:        u32,
    /// Canonical tool result payload. Carries the structured output, error
    /// state, and supported media/artifact fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result:  Option<ToolResult>,
    /// Turn that owned this tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id:      Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentErrorProps {
    pub error: Value,
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentWarningProps {
    pub kind:    String,
    pub message: String,
    pub details: Value,
    pub visit:   u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLoopDetectedProps {
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTurnLimitReachedProps {
    pub max_turns: usize,
    pub visit:     u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSteeringInjectedProps {
    pub text:  String,
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPairUserMessageProps {
    pub pair_id:           PairId,
    pub message_id:        PairMessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    pub text:              String,
    pub visit:             u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPairSystemMessageProps {
    pub pair_id: PairId,
    pub kind:    PairSystemMessageKind,
    pub text:    String,
    pub visit:   u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInterruptInjectedProps {
    pub visit: u32,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSteerBufferedProps {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSteerDroppedReason {
    QueueFull,
    RunEnded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSteerDroppedProps {
    pub reason: AgentSteerDroppedReason,
    pub count:  u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCompactionStartedProps {
    pub estimated_tokens:    usize,
    pub context_window_size: usize,
    pub visit:               u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCompactionCompletedProps {
    pub original_turn_count:    usize,
    pub preserved_turn_count:   usize,
    pub summary_token_estimate: usize,
    pub tracked_file_count:     usize,
    pub visit:                  u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLlmRetryProps {
    pub provider:   String,
    pub model:      String,
    pub attempt:    usize,
    pub delay_secs: f64,
    pub error:      Value,
    pub visit:      u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentContextWindowSnapshotProps {
    pub stage_id: StageId,
    pub visit:    u32,
    #[serde(flatten)]
    pub snapshot: StageContextWindowProjection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubSpawnedProps {
    pub agent_id: String,
    pub depth:    usize,
    pub task:     String,
    pub visit:    u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubCompletedProps {
    pub agent_id:   String,
    pub depth:      usize,
    pub success:    bool,
    pub turns_used: usize,
    pub visit:      u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubFailedProps {
    pub agent_id: String,
    pub depth:    usize,
    pub error:    Value,
    pub visit:    u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSubClosedProps {
    pub agent_id: String,
    pub depth:    usize,
    pub visit:    u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMcpReadyProps {
    pub server_name: String,
    pub tool_count:  usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools:       Vec<AgentMcpToolSummary>,
    pub visit:       u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMcpToolSummary {
    pub name:          String,
    pub original_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMcpFailedProps {
    pub server_name: String,
    pub error:       String,
    pub visit:       u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMemoryLoadedProps {
    pub provider_profile:   String,
    pub files:              Vec<AgentMemoryFileProps>,
    pub total_loaded_bytes: usize,
    pub budget_bytes:       usize,
    pub visit:              u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMemoryFileProps {
    pub path:         String,
    pub byte_count:   usize,
    pub loaded_bytes: usize,
    pub truncated:    bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSkillsDiscoveredProps {
    pub provider_profile: String,
    pub source_dirs:      Vec<String>,
    pub skills:           Vec<AgentSkillSummary>,
    pub visit:            u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSkillSummary {
    pub name:        String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSkillActivationSource {
    Slash,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSkillActivatedProps {
    pub skill_name: String,
    pub source:     AgentSkillActivationSource,
    pub visit:      u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::transcript::{ContentPart, MessageKind, MessageSource, TranscriptMessage};

    fn sample_model_ref() -> ModelRef {
        ModelRef {
            provider: fabro_model::ProviderId::openai(),
            model_id: "gpt-5".to_string(),
            speed:    None,
        }
    }

    #[test]
    fn agent_message_props_back_compat_deserializes_without_message_field() {
        // Legacy payload from before the transcript migration.
        let v = json!({
            "text": "hello",
            "model": {"provider": "openai", "model_id": "gpt-5"},
            "billing": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15,
            },
            "tool_call_count": 0,
            "visit": 1,
        });
        let props: AgentMessageProps = serde_json::from_value(v).unwrap();
        assert_eq!(props.text, "hello");
        assert!(props.message.is_none());
    }

    #[test]
    fn agent_message_props_carries_canonical_transcript_message() {
        let msg = TranscriptMessage::new(MessageKind::Agent, MessageSource::ProviderAnswer, vec![
            ContentPart::text("ok"),
        ]);
        let props = AgentMessageProps {
            text:            "ok".to_string(),
            model:           sample_model_ref(),
            billing:         BilledTokenCounts::default(),
            tool_call_count: 0,
            visit:           1,
            message:         Some(msg.clone()),
        };
        let v = serde_json::to_value(&props).unwrap();
        assert_eq!(v["message"]["kind"], "agent");
        assert_eq!(v["message"]["source"], "provider_answer");
        let back: AgentMessageProps = serde_json::from_value(v).unwrap();
        assert_eq!(back, props);
    }

    #[test]
    fn agent_tool_started_props_back_compat_deserializes_without_canonical_fields() {
        let v = json!({
            "tool_name": "Bash",
            "tool_call_id": "call_1",
            "arguments": {"cmd": "ls"},
            "visit": 1,
        });
        let props: AgentToolStartedProps = serde_json::from_value(v).unwrap();
        assert_eq!(props.tool_name, "Bash");
        assert!(props.tool_call.is_none());
        assert!(props.turn_id.is_none());
        assert!(props.parent_message_id.is_none());
    }

    #[test]
    fn agent_tool_started_props_carries_canonical_tool_call_and_linkage() {
        let mut tc = ToolCall::new("call_1", "Bash", json!({"cmd": "ls"}));
        tc.provider_metadata = Some(json!({"thought_signature": "sig"}));
        let parent = MessageId::new();
        let turn = TurnId::new();
        let props = AgentToolStartedProps {
            tool_name:         "Bash".to_string(),
            tool_call_id:      "call_1".to_string(),
            arguments:         json!({"cmd": "ls"}),
            visit:             1,
            tool_call:         Some(tc.clone()),
            turn_id:           Some(turn),
            parent_message_id: Some(parent),
        };
        let v = serde_json::to_value(&props).unwrap();
        assert_eq!(
            v["tool_call"]["provider_metadata"]["thought_signature"],
            "sig"
        );
        assert_eq!(v["turn_id"], turn.to_string());
        assert_eq!(v["parent_message_id"], parent.to_string());
        let back: AgentToolStartedProps = serde_json::from_value(v).unwrap();
        assert_eq!(back, props);
    }

    #[test]
    fn agent_tool_completed_props_back_compat_deserializes_without_canonical_fields() {
        let v = json!({
            "tool_name": "Bash",
            "tool_call_id": "call_1",
            "output": "ok\n",
            "is_error": false,
            "visit": 1,
        });
        let props: AgentToolCompletedProps = serde_json::from_value(v).unwrap();
        assert!(props.tool_result.is_none());
        assert!(props.turn_id.is_none());
    }

    #[test]
    fn agent_tool_completed_props_carries_canonical_tool_result() {
        let tr = ToolResult::success("call_1", json!({"stdout": "ok"}));
        let turn = TurnId::new();
        let props = AgentToolCompletedProps {
            tool_name:    "Bash".to_string(),
            tool_call_id: "call_1".to_string(),
            output:       json!({"stdout": "ok"}),
            is_error:     false,
            visit:        1,
            tool_result:  Some(tr.clone()),
            turn_id:      Some(turn),
        };
        let v = serde_json::to_value(&props).unwrap();
        assert_eq!(v["tool_result"]["content"]["stdout"], "ok");
        let back: AgentToolCompletedProps = serde_json::from_value(v).unwrap();
        assert_eq!(back, props);
    }
}
