use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fabro_llm::types::ReasoningEffort;
use fabro_mcp::config::McpServerConfig;

/// Callback invoked before each tool execution. Return `Ok(())` to allow,
/// `Err(message)` to deny with the given message.
pub type ToolApprovalFn = Arc<dyn Fn(&str, &serde_json::Value) -> Result<(), String> + Send + Sync>;

/// Decision returned by a [`ToolHookCallback`] before a tool executes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolHookDecision {
    /// Allow the tool call to proceed.
    #[default]
    Proceed,
    /// Block the tool call with the given reason.
    Block { reason: String },
}

/// Async callback trait invoked around tool execution.
#[async_trait::async_trait]
pub trait ToolHookCallback: Send + Sync {
    /// Called before a tool executes. Return [`ToolHookDecision::Proceed`] to
    /// allow or [`ToolHookDecision::Block`] to deny.
    async fn pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> ToolHookDecision;

    /// Called after a tool executes successfully.
    async fn post_tool_use(&self, tool_name: &str, tool_call_id: &str, tool_output: &str);

    /// Called after a tool execution fails.
    async fn post_tool_use_failure(&self, tool_name: &str, tool_call_id: &str, error: &str);
}

/// Adapter that wraps a [`ToolApprovalFn`] and implements [`ToolHookCallback`].
pub struct ToolApprovalAdapter(pub ToolApprovalFn);

#[async_trait::async_trait]
impl ToolHookCallback for ToolApprovalAdapter {
    async fn pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> ToolHookDecision {
        match (self.0)(tool_name, tool_input) {
            Ok(()) => ToolHookDecision::Proceed,
            Err(reason) => ToolHookDecision::Block { reason },
        }
    }

    async fn post_tool_use(&self, _tool_name: &str, _tool_call_id: &str, _tool_output: &str) {}

    async fn post_tool_use_failure(&self, _tool_name: &str, _tool_call_id: &str, _error: &str) {}
}

#[derive(Clone)]
pub struct SessionConfig {
    pub max_turns: usize,
    pub max_tool_rounds_per_input: usize,
    pub default_command_timeout_ms: u64,
    pub max_command_timeout_ms: u64,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub speed: Option<String>,
    pub tool_output_limits: HashMap<String, usize>,
    pub tool_line_limits: HashMap<String, usize>,
    /// Override the provider's default max_tokens when set.
    /// Node-level attribute takes priority over the model catalog default.
    pub max_tokens: Option<i64>,
    pub enable_loop_detection: bool,
    pub loop_detection_window: usize,
    pub max_subagent_depth: usize,
    pub git_root: Option<String>,
    pub user_instructions: Option<String>,
    /// Async hook callbacks invoked around tool execution.
    pub tool_hooks: Option<Arc<dyn ToolHookCallback>>,
    pub enable_context_compaction: bool,
    pub compaction_threshold_percent: usize,
    pub compaction_preserve_turns: usize,
    /// Skill directories. `None` = use convention defaults, `Some(dirs)` = use these instead.
    pub skill_dirs: Option<Vec<String>>,
    /// MCP server configurations to connect to on session startup.
    pub mcp_servers: Vec<McpServerConfig>,
    /// Wall-clock timeout for the entire `process_input` call.
    /// When set, the session's cancel token is triggered after this duration.
    pub wall_clock_timeout: Option<Duration>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("max_turns", &self.max_turns)
            .field("max_tool_rounds_per_input", &self.max_tool_rounds_per_input)
            .field(
                "default_command_timeout_ms",
                &self.default_command_timeout_ms,
            )
            .field("max_command_timeout_ms", &self.max_command_timeout_ms)
            .field("max_tokens", &self.max_tokens)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("speed", &self.speed)
            .field("tool_output_limits", &self.tool_output_limits)
            .field("tool_line_limits", &self.tool_line_limits)
            .field("enable_loop_detection", &self.enable_loop_detection)
            .field("loop_detection_window", &self.loop_detection_window)
            .field("max_subagent_depth", &self.max_subagent_depth)
            .field("git_root", &self.git_root)
            .field("user_instructions", &self.user_instructions)
            .field(
                "tool_hooks",
                &self.tool_hooks.as_ref().map(|_| "<callback>"),
            )
            .field("enable_context_compaction", &self.enable_context_compaction)
            .field(
                "compaction_threshold_percent",
                &self.compaction_threshold_percent,
            )
            .field("compaction_preserve_turns", &self.compaction_preserve_turns)
            .field("skill_dirs", &self.skill_dirs)
            .field("mcp_servers", &self.mcp_servers.len())
            .field("wall_clock_timeout", &self.wall_clock_timeout)
            .finish()
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_turns: 0,
            max_tool_rounds_per_input: 0,
            default_command_timeout_ms: 10_000,
            max_command_timeout_ms: 600_000,
            max_tokens: None,
            reasoning_effort: None,
            speed: None,
            tool_output_limits: HashMap::new(),
            tool_line_limits: HashMap::new(),
            enable_loop_detection: true,
            loop_detection_window: 10,
            max_subagent_depth: 1,
            git_root: None,
            user_instructions: None,
            tool_hooks: None,
            enable_context_compaction: true,
            compaction_threshold_percent: 80,
            compaction_preserve_turns: 6,
            skill_dirs: None,
            mcp_servers: Vec::new(),
            wall_clock_timeout: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = SessionConfig::default();
        assert_eq!(config.max_turns, 0);
        assert_eq!(config.max_tool_rounds_per_input, 0);
        assert_eq!(config.default_command_timeout_ms, 10_000);
        assert_eq!(config.max_command_timeout_ms, 600_000);
        assert!(config.reasoning_effort.is_none());
        assert!(config.tool_output_limits.is_empty());
        assert!(config.tool_line_limits.is_empty());
        assert!(config.enable_loop_detection);
        assert_eq!(config.loop_detection_window, 10);
        assert_eq!(config.max_subagent_depth, 1);
        assert!(config.user_instructions.is_none());
        assert!(config.mcp_servers.is_empty());
        assert!(config.wall_clock_timeout.is_none());
    }

    #[test]
    fn default_config_has_compaction_enabled() {
        let config = SessionConfig::default();
        assert!(config.enable_context_compaction);
        assert_eq!(config.compaction_threshold_percent, 80);
        assert_eq!(config.compaction_preserve_turns, 6);
    }

    #[test]
    fn config_with_custom_values() {
        let config = SessionConfig {
            max_turns: 50,
            reasoning_effort: Some(ReasoningEffort::High),
            ..Default::default()
        };
        assert_eq!(config.max_turns, 50);
        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(config.max_tool_rounds_per_input, 0);
    }

    #[test]
    fn tool_hook_decision_default_is_proceed() {
        assert_eq!(ToolHookDecision::default(), ToolHookDecision::Proceed);
    }

    #[tokio::test]
    async fn tool_approval_adapter_allows() {
        let approval: ToolApprovalFn = Arc::new(|_name, _args| Ok(()));
        let adapter = ToolApprovalAdapter(approval);
        let decision = adapter.pre_tool_use("shell", &serde_json::json!({})).await;
        assert_eq!(decision, ToolHookDecision::Proceed);
    }

    #[tokio::test]
    async fn tool_approval_adapter_blocks() {
        let approval: ToolApprovalFn = Arc::new(|_name, _args| Err("denied".to_string()));
        let adapter = ToolApprovalAdapter(approval);
        let decision = adapter.pre_tool_use("shell", &serde_json::json!({})).await;
        assert_eq!(
            decision,
            ToolHookDecision::Block {
                reason: "denied".to_string()
            }
        );
    }

    #[tokio::test]
    async fn tool_approval_adapter_post_is_noop() {
        let approval: ToolApprovalFn = Arc::new(|_name, _args| Ok(()));
        let adapter = ToolApprovalAdapter(approval);
        // These should not panic
        adapter.post_tool_use("shell", "call_1", "output").await;
        adapter
            .post_tool_use_failure("shell", "call_1", "error")
            .await;
    }
}
