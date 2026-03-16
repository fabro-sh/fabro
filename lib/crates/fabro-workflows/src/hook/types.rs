use serde::{Deserialize, Serialize};

/// Lifecycle events that can trigger user-defined hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    RunStart,
    RunComplete,
    RunFailed,
    StageStart,
    StageComplete,
    StageFailed,
    StageRetrying,
    EdgeSelected,
    ParallelStart,
    ParallelComplete,
    /// Reserved: hooks for this event are not yet invoked by the engine.
    SandboxReady,
    /// Reserved: hooks for this event are not yet invoked by the engine.
    SandboxCleanup,
    CheckpointSaved,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
}

impl HookEvent {
    /// Whether hooks for this event block execution by default.
    #[must_use]
    pub fn is_blocking_by_default(self) -> bool {
        matches!(
            self,
            Self::RunStart | Self::StageStart | Self::EdgeSelected | Self::PreToolUse
        )
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RunStart => "run_start",
            Self::RunComplete => "run_complete",
            Self::RunFailed => "run_failed",
            Self::StageStart => "stage_start",
            Self::StageComplete => "stage_complete",
            Self::StageFailed => "stage_failed",
            Self::StageRetrying => "stage_retrying",
            Self::EdgeSelected => "edge_selected",
            Self::ParallelStart => "parallel_start",
            Self::ParallelComplete => "parallel_complete",
            Self::SandboxReady => "sandbox_ready",
            Self::SandboxCleanup => "sandbox_cleanup",
            Self::CheckpointSaved => "checkpoint_saved",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PostToolUseFailure => "post_tool_use_failure",
        })
    }
}

/// Rich JSON payload sent to hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    pub event: HookEvent,
    pub run_id: String,
    pub workflow_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl HookContext {
    /// Populate node-related fields from a graph `Node`.
    pub fn set_node(&mut self, node: &crate::graph::Node) {
        self.node_id = Some(node.id.clone());
        self.node_label = Some(node.label().to_string());
        self.handler_type = node.handler_type().map(String::from);
    }

    #[must_use]
    pub fn new(event: HookEvent, run_id: String, workflow_name: String) -> Self {
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
    pub ok: bool,
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
            (Self::Skip { .. }, _) | (Self::Override { .. }, _) => self,
            (_, Self::Skip { .. }) | (_, Self::Override { .. }) => other,
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
    pub hook_name: Option<String>,
    pub decision: HookDecision,
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_serde_round_trip() {
        let events = [
            HookEvent::RunStart,
            HookEvent::RunComplete,
            HookEvent::RunFailed,
            HookEvent::StageStart,
            HookEvent::StageComplete,
            HookEvent::StageFailed,
            HookEvent::StageRetrying,
            HookEvent::EdgeSelected,
            HookEvent::ParallelStart,
            HookEvent::ParallelComplete,
            HookEvent::SandboxReady,
            HookEvent::SandboxCleanup,
            HookEvent::CheckpointSaved,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::PostToolUseFailure,
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn hook_event_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&HookEvent::RunStart).unwrap(),
            "\"run_start\""
        );
        assert_eq!(
            serde_json::to_string(&HookEvent::StageRetrying).unwrap(),
            "\"stage_retrying\""
        );
    }

    #[test]
    fn hook_event_display() {
        assert_eq!(HookEvent::RunStart.to_string(), "run_start");
        assert_eq!(HookEvent::CheckpointSaved.to_string(), "checkpoint_saved");
    }

    #[test]
    fn hook_event_blocking_defaults() {
        assert!(HookEvent::RunStart.is_blocking_by_default());
        assert!(HookEvent::StageStart.is_blocking_by_default());
        assert!(HookEvent::EdgeSelected.is_blocking_by_default());
        assert!(!HookEvent::RunComplete.is_blocking_by_default());
        assert!(!HookEvent::StageFailed.is_blocking_by_default());
        assert!(!HookEvent::CheckpointSaved.is_blocking_by_default());
    }

    #[test]
    fn hook_context_serde_round_trip() {
        let ctx = HookContext {
            event: HookEvent::StageStart,
            run_id: "run-123".into(),
            workflow_name: "test-wf".into(),
            cwd: Some("/tmp".into()),
            node_id: Some("plan".into()),
            node_label: Some("Plan".into()),
            handler_type: Some("agent".into()),
            status: None,
            edge_from: None,
            edge_to: None,
            edge_label: None,
            failure_reason: None,
            attempt: Some(1),
            max_attempts: Some(3),
            tool_name: None,
            tool_input: None,
            tool_call_id: None,
            tool_output: None,
            error_message: None,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: HookContext = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event, HookEvent::StageStart);
        assert_eq!(back.run_id, "run-123");
        assert_eq!(back.node_id.as_deref(), Some("plan"));
    }

    #[test]
    fn hook_context_omits_none_fields() {
        let ctx = HookContext::new(HookEvent::RunStart, "run-1".into(), "wf".into());
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
    fn pre_tool_use_serde_round_trip() {
        let json = serde_json::to_string(&HookEvent::PreToolUse).unwrap();
        assert_eq!(json, "\"pre_tool_use\"");
        let back: HookEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, HookEvent::PreToolUse);
    }

    #[test]
    fn pre_tool_use_is_blocking_by_default() {
        assert!(HookEvent::PreToolUse.is_blocking_by_default());
    }

    #[test]
    fn post_tool_use_is_not_blocking_by_default() {
        assert!(!HookEvent::PostToolUse.is_blocking_by_default());
    }

    #[test]
    fn post_tool_use_failure_is_not_blocking_by_default() {
        assert!(!HookEvent::PostToolUseFailure.is_blocking_by_default());
    }

    #[test]
    fn hook_context_with_tool_fields() {
        let mut ctx = HookContext::new(HookEvent::PreToolUse, "run-1".into(), "wf".into());
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
        let mut ctx = HookContext::new(HookEvent::PostToolUse, "run-1".into(), "wf".into());
        ctx.tool_name = Some("shell".into());
        ctx.tool_output = Some("file1.txt\nfile2.txt".into());
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("\"tool_output\""));
        // error_message should be omitted
        assert!(!json.contains("\"error_message\""));
    }
}
