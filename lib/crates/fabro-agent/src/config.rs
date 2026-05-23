use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fabro_llm::types::{ReasoningEffort, Speed};
use fabro_mcp::config::McpServerSettings;
use fabro_types::PermissionLevel;

/// Callback invoked before each tool execution. Return `Ok(())` to allow,
/// `Err(message)` to deny with the given message.
pub type ToolApprovalFn = Arc<dyn Fn(&str, &serde_json::Value) -> Result<(), String> + Send + Sync>;

/// Static access classification for a registered tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAccess {
    /// The tool can be exposed and executed without an approval step.
    Allowed,
    /// The tool can be exposed only when the session has an approval path.
    RequiresApproval,
    /// The tool must not be exposed or executed.
    Denied,
}

impl ToolAccess {
    #[must_use]
    pub const fn is_exposed(self, mode: ToolExposureMode) -> bool {
        match self {
            Self::Allowed => true,
            Self::RequiresApproval => matches!(mode, ToolExposureMode::IncludeRequiresApproval),
            Self::Denied => false,
        }
    }
}

/// Controls whether approval-required tools are included in LLM tool schemas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolExposureMode {
    /// Expose only tools that can run without an approval path.
    #[default]
    AutoApprovedOnly,
    /// Expose tools classified as [`ToolAccess::RequiresApproval`].
    IncludeRequiresApproval,
}

/// Static policy used to decide which tools are effectively available.
///
/// This policy is intentionally name-only. Keep argument-sensitive approval,
/// logging, telemetry, and async decisions in [`ToolHookCallback`].
pub trait ToolAccessPolicy: Send + Sync {
    fn access_for_tool(&self, tool_name: &str) -> ToolAccess;
}

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
pub struct SessionOptions {
    pub max_turns: usize,
    pub max_tool_rounds_per_input: usize,
    pub default_command_timeout_ms: u64,
    pub max_command_timeout_ms: u64,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub speed: Option<Speed>,
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
    /// Static policy used to filter advertised tools and block hidden calls.
    /// `None` preserves legacy behavior: all registered tools are exposed.
    pub tool_access_policy: Option<Arc<dyn ToolAccessPolicy>>,
    /// Agent tool permission level applied when the session started.
    pub permission_level: Option<PermissionLevel>,
    /// Tool schema exposure mode used when `tool_access_policy` is set.
    pub tool_exposure_mode: ToolExposureMode,
    pub enable_context_compaction: bool,
    pub compaction_threshold_percent: usize,
    pub compaction_preserve_turns: usize,
    /// Skill directories. `None` = use convention defaults, `Some(dirs)` = use
    /// these instead.
    pub skill_dirs: Option<Vec<String>>,
    /// MCP server configurations to connect to on session startup.
    pub mcp_servers: Vec<McpServerSettings>,
    /// Wall-clock timeout for the entire `process_input` call.
    /// When set, the session's cancel token is triggered after this duration.
    pub wall_clock_timeout: Option<Duration>,
}

impl std::fmt::Debug for SessionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionOptions")
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
            .field(
                "tool_access_policy",
                &self.tool_access_policy.as_ref().map(|_| "<policy>"),
            )
            .field("permission_level", &self.permission_level)
            .field("tool_exposure_mode", &self.tool_exposure_mode)
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

impl Default for SessionOptions {
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
            tool_access_policy: None,
            permission_level: None,
            tool_exposure_mode: ToolExposureMode::AutoApprovedOnly,
            enable_context_compaction: true,
            compaction_threshold_percent: 80,
            compaction_preserve_turns: 6,
            skill_dirs: None,
            mcp_servers: Vec::new(),
            wall_clock_timeout: None,
        }
    }
}

impl SessionOptions {
    #[must_use]
    pub fn tool_access_for(&self, tool_name: &str) -> ToolAccess {
        self.tool_access_policy
            .as_ref()
            .map_or(ToolAccess::Allowed, |policy| {
                policy.access_for_tool(tool_name)
            })
    }

    #[must_use]
    pub fn exposes_tool(&self, tool_name: &str) -> bool {
        self.tool_access_policy.as_ref().is_none_or(|policy| {
            policy
                .access_for_tool(tool_name)
                .is_exposed(self.tool_exposure_mode)
        })
    }

    #[must_use]
    pub fn tool_access_denial_reason(&self, tool_name: &str) -> Option<String> {
        self.tool_access_policy.as_ref()?;
        match self.tool_access_for(tool_name) {
            ToolAccess::Allowed => None,
            ToolAccess::RequiresApproval
                if matches!(
                    self.tool_exposure_mode,
                    ToolExposureMode::IncludeRequiresApproval
                ) =>
            {
                None
            }
            ToolAccess::RequiresApproval => Some(format!(
                "{tool_name} tool requires approval, but this session does not expose approval-required tools"
            )),
            ToolAccess::Denied => Some(format!("{tool_name} tool denied by tool access policy")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticToolPolicy(ToolAccess);

    impl ToolAccessPolicy for StaticToolPolicy {
        fn access_for_tool(&self, _tool_name: &str) -> ToolAccess {
            self.0
        }
    }

    #[test]
    fn default_config_values() {
        let config = SessionOptions::default();
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
        assert!(config.tool_access_policy.is_none());
        assert!(config.permission_level.is_none());
        assert_eq!(
            config.tool_exposure_mode,
            ToolExposureMode::AutoApprovedOnly
        );
        assert!(config.mcp_servers.is_empty());
        assert!(config.wall_clock_timeout.is_none());
    }

    #[test]
    fn default_config_has_compaction_enabled() {
        let config = SessionOptions::default();
        assert!(config.enable_context_compaction);
        assert_eq!(config.compaction_threshold_percent, 80);
        assert_eq!(config.compaction_preserve_turns, 6);
    }

    #[test]
    fn config_with_custom_values() {
        let config = SessionOptions {
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

    #[test]
    fn no_tool_access_policy_exposes_tools_by_default() {
        let config = SessionOptions::default();
        assert_eq!(config.tool_access_for("shell"), ToolAccess::Allowed);
        assert!(config.exposes_tool("shell"));
        assert!(config.tool_access_denial_reason("shell").is_none());
    }

    #[test]
    fn denied_tool_access_has_denial_reason() {
        let config = SessionOptions {
            tool_access_policy: Some(Arc::new(StaticToolPolicy(ToolAccess::Denied))),
            ..SessionOptions::default()
        };
        let reason = config
            .tool_access_denial_reason("shell")
            .expect("denied tool should have reason");
        assert!(reason.contains("denied by tool access policy"));
        assert!(!config.exposes_tool("shell"));
    }

    #[test]
    fn approval_required_tools_follow_exposure_mode() {
        let config = SessionOptions {
            tool_access_policy: Some(Arc::new(StaticToolPolicy(ToolAccess::RequiresApproval))),
            tool_exposure_mode: ToolExposureMode::AutoApprovedOnly,
            ..SessionOptions::default()
        };
        assert!(!config.exposes_tool("shell"));
        assert!(
            config
                .tool_access_denial_reason("shell")
                .expect("hidden approval tool should have reason")
                .contains("requires approval")
        );

        let config = SessionOptions {
            tool_exposure_mode: ToolExposureMode::IncludeRequiresApproval,
            ..config
        };
        assert!(config.exposes_tool("shell"));
        assert!(config.tool_access_denial_reason("shell").is_none());
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
        assert_eq!(decision, ToolHookDecision::Block {
            reason: "denied".to_string(),
        });
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
