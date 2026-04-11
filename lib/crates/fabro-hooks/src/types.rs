use fabro_types::RunId;
use serde::{Deserialize, Serialize};

pub use crate::config::HookEvent;

/// Rich JSON payload sent to hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    pub event:          HookEvent,
    pub run_id:         RunId,
    pub workflow_name:  String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_label:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_type:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_from:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_to:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_label:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt:        Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts:   Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input:     Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message:  Option<String>,
}

impl HookContext {
    #[must_use]
    pub fn new(event: HookEvent, run_id: RunId, workflow_name: String) -> Self {
        Self {
            event,
            run_id,
            workflow_name,
            cwd: None,
            node_id: None,
            node_label: None,
            handler_type: None,
            status: None,
            edge_from: None,
            edge_to: None,
            edge_label: None,
            failure_reason: None,
            attempt: None,
            max_attempts: None,
            tool_name: None,
            tool_input: None,
            tool_call_id: None,
            tool_output: None,
            error_message: None,
        }
    }
}

/// Response returned by prompt/agent hooks from the LLM.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PromptHookResponse {
    pub ok:     bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Decision returned by blocking hooks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum HookDecision {
    #[default]
    Proceed,
    Skip {
        #[serde(default)]
        reason: Option<String>,
    },
    Block {
        #[serde(default)]
        reason: Option<String>,
    },
    Override {
        edge_to: String,
    },
}

impl HookDecision {
    /// Merge two decisions. Block > Skip/Override > Proceed.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        match (&self, &other) {
            (Self::Block { .. }, _) => self,
            (_, Self::Block { .. }) => other,
            (Self::Skip { .. } | Self::Override { .. }, _) => self,
            (_, Self::Skip { .. } | Self::Override { .. }) => other,
            _ => Self::Proceed,
        }
    }

    #[must_use]
    pub fn is_proceed(&self) -> bool {
        matches!(self, Self::Proceed)
    }
}

/// Result from executing a single hook.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub hook_name:   Option<String>,
    pub decision:    HookDecision,
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use fabro_types::fixtures;

    use super::*;

    #[test]
    fn hook_context_serde_round_trip() {
        let ctx = HookContext {
            event:          HookEvent::StageStart,
            run_id:         fixtures::RUN_1,
            workflow_name:  "test-wf".into(),
            cwd:            Some("/tmp".into()),
            node_id:        Some("plan".into()),
            node_label:     Some("Plan".into()),
            handler_type:   Some("agent".into()),
            status:         None,
            edge_from:      None,
            edge_to:        None,
            edge_label:     None,
            failure_reason: None,
            attempt:        Some(1),
            max_attempts:   Some(3),
            tool_name:      None,
            tool_input:     None,
            tool_call_id:   None,
            tool_output:    None,
            error_message:  None,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: HookContext = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event, HookEvent::StageStart);
        assert_eq!(back.run_id, fixtures::RUN_1);
        assert_eq!(back.node_id.as_deref(), Some("plan"));
    }

    #[test]
    fn hook_context_omits_none_fields() {
        let ctx = HookContext::new(HookEvent::RunStart, fixtures::RUN_1, "wf".into());
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("node_id"));
        assert!(!json.contains("failure_reason"));
    }

    #[test]
    fn hook_decision_serde_round_trip() {
        let decisions = [
            HookDecision::Proceed,
            HookDecision::Skip {
                reason: Some("not needed".into()),
            },
            HookDecision::Block {
                reason: Some("forbidden".into()),
            },
            HookDecision::Override {
                edge_to: "node_b".into(),
            },
        ];
        for decision in decisions {
            let json = serde_json::to_string(&decision).unwrap();
            let back: HookDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(decision, back);
        }
    }

    #[test]
    fn hook_decision_merge_block_wins() {
        let block = HookDecision::Block {
            reason: Some("no".into()),
        };
        let skip = HookDecision::Skip {
            reason: Some("skip".into()),
        };
        let proceed = HookDecision::Proceed;

        assert!(matches!(
            proceed.clone().merge(block.clone()),
            HookDecision::Block { .. }
        ));
        assert!(matches!(
            block.clone().merge(skip.clone()),
            HookDecision::Block { .. }
        ));
        assert!(matches!(
            skip.clone().merge(block.clone()),
            HookDecision::Block { .. }
        ));
    }

    #[test]
    fn hook_decision_merge_skip_over_proceed() {
        let skip = HookDecision::Skip {
            reason: Some("skip".into()),
        };
        let proceed = HookDecision::Proceed;

        assert!(matches!(
            proceed.clone().merge(skip.clone()),
            HookDecision::Skip { .. }
        ));
        assert!(matches!(skip.merge(proceed), HookDecision::Skip { .. }));
    }

    #[test]
    fn hook_decision_merge_first_non_proceed_wins() {
        let skip = HookDecision::Skip {
            reason: Some("a".into()),
        };
        let override_d = HookDecision::Override {
            edge_to: "x".into(),
        };
        // First non-Proceed wins when no Block
        assert!(matches!(skip.merge(override_d), HookDecision::Skip { .. }));
    }

    #[test]
    fn hook_decision_default_is_proceed() {
        assert_eq!(HookDecision::default(), HookDecision::Proceed);
    }

    #[test]
    fn prompt_hook_response_ok_true() {
        let resp: PromptHookResponse = serde_json::from_str(r#"{"ok": true}"#).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.reason, None);
    }

    #[test]
    fn prompt_hook_response_ok_false_with_reason() {
        let resp: PromptHookResponse =
            serde_json::from_str(r#"{"ok": false, "reason": "not ready"}"#).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.reason.as_deref(), Some("not ready"));
    }

    #[test]
    fn hook_context_with_tool_fields() {
        let mut ctx = HookContext::new(HookEvent::PreToolUse, fixtures::RUN_1, "wf".into());
        ctx.tool_name = Some("shell".into());
        ctx.tool_input = Some(serde_json::json!({"command": "ls"}));
        ctx.tool_call_id = Some("call_123".into());
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("\"tool_name\":\"shell\""));
        assert!(json.contains("\"tool_call_id\":\"call_123\""));
        assert!(json.contains("\"tool_input\""));
    }

    #[test]
    fn hook_context_tool_output_serializes() {
        let mut ctx = HookContext::new(HookEvent::PostToolUse, fixtures::RUN_1, "wf".into());
        ctx.tool_name = Some("shell".into());
        ctx.tool_output = Some("file1.txt\nfile2.txt".into());
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("\"tool_output\""));
        // error_message should be omitted
        assert!(!json.contains("\"error_message\""));
    }
}
