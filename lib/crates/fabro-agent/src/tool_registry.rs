use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use fabro_llm::types::ToolDefinition;
use tokio_util::sync::CancellationToken;

use crate::config::{ToolAccessPolicy, ToolExposureMode};
use crate::sandbox::Sandbox;
use crate::session::ToolEnvProvider;
use crate::types::AgentEvent;

/// Narrow handle a tool uses to publish typed agent events (e.g. todo
/// mutations) onto the active session's event stream. The implementation
/// must tag emitted events with the same `session_id` / `parent_session_id`
/// the session is using.
pub trait AgentEventEmitter: Send + Sync {
    fn emit(&self, event: AgentEvent);
}

pub struct ToolContext {
    pub env:                 Arc<dyn Sandbox>,
    pub cancel:              CancellationToken,
    pub tool_env_provider:   Option<Arc<dyn ToolEnvProvider>>,
    /// Emitting session's ID. `None` when a tool is invoked outside of a
    /// session (e.g. ad-hoc unit tests).
    pub session_id:          Option<String>,
    /// Root session for this session's agent tree. Equal to `session_id`
    /// for the root agent; subagent sessions inherit the parent's root.
    pub root_session_id:     Option<String>,
    /// Active model-native tool call ID, when available.
    pub tool_call_id:        Option<String>,
    /// Narrow emitter for typed agent events (todo mutations and similar).
    pub agent_event_emitter: Option<Arc<dyn AgentEventEmitter>>,
}

impl ToolContext {
    pub async fn resolve_tool_env(&self) -> anyhow::Result<Option<HashMap<String, String>>> {
        match &self.tool_env_provider {
            Some(provider) => Ok(Some(provider.resolve().await?)),
            None => Ok(None),
        }
    }

    /// Publish an agent event using the bound emitter. No-op when the
    /// context has no emitter (test fixtures).
    pub fn emit_agent_event(&self, event: AgentEvent) {
        if let Some(emitter) = self.agent_event_emitter.as_ref() {
            emitter.emit(event);
        }
    }
}

pub type ToolExecutor = Arc<
    dyn Fn(
            serde_json::Value,
            ToolContext,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub executor:   ToolExecutor,
    pub source:     ToolSource,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ToolSource {
    #[default]
    Native,
    Mcp {
        server_name: String,
    },
    Skill,
}

#[derive(Clone)]
pub struct ToolDefinitionWithSource {
    pub definition: ToolDefinition,
    pub source:     ToolSource,
}

pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: RegisteredTool) {
        self.tools.insert(tool.definition.name.clone(), tool);
    }

    pub fn unregister(&mut self, name: &str) -> Option<RegisteredTool> {
        self.tools.remove(name)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }

    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition.clone()).collect()
    }

    #[must_use]
    pub fn definitions_with_source(&self) -> Vec<ToolDefinitionWithSource> {
        self.tools
            .values()
            .map(|tool| ToolDefinitionWithSource {
                definition: tool.definition.clone(),
                source:     tool.source.clone(),
            })
            .collect()
    }

    #[must_use]
    pub fn definitions_for_policy(
        &self,
        policy: Option<&dyn ToolAccessPolicy>,
        exposure_mode: ToolExposureMode,
    ) -> Vec<ToolDefinition> {
        self.definitions_with_source_for_policy(policy, exposure_mode)
            .into_iter()
            .map(|tool| tool.definition)
            .collect()
    }

    #[must_use]
    pub fn definitions_with_source_for_policy(
        &self,
        policy: Option<&dyn ToolAccessPolicy>,
        exposure_mode: ToolExposureMode,
    ) -> Vec<ToolDefinitionWithSource> {
        self.tools
            .values()
            .filter(|tool| {
                policy.is_none_or(|policy| {
                    policy
                        .access_for_tool(&tool.definition.name)
                        .is_exposed(exposure_mode)
                })
            })
            .map(|tool| ToolDefinitionWithSource {
                definition: tool.definition.clone(),
                source:     tool.source.clone(),
            })
            .collect()
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToolAccess, ToolAccessPolicy, ToolExposureMode};
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;

    struct NamedPolicy {
        decisions: HashMap<String, ToolAccess>,
    }

    impl NamedPolicy {
        fn new(decisions: impl IntoIterator<Item = (&'static str, ToolAccess)>) -> Self {
            Self {
                decisions: decisions
                    .into_iter()
                    .map(|(name, access)| (name.to_string(), access))
                    .collect(),
            }
        }
    }

    impl ToolAccessPolicy for NamedPolicy {
        fn access_for_tool(&self, tool_name: &str) -> ToolAccess {
            self.decisions
                .get(tool_name)
                .copied()
                .unwrap_or(ToolAccess::Denied)
        }
    }

    fn make_tool(name: &str) -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name:        name.into(),
                description: format!("Tool {name}"),
                parameters:  serde_json::json!({"type": "object"}),
            },
            executor:   Arc::new(|_args, _ctx| Box::pin(async { Ok("ok".into()) })),
            source:     ToolSource::Native,
        }
    }

    #[test]
    fn register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("read_file"));

        let tool = registry.get("read_file");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().definition.name, "read_file");
    }

    #[test]
    fn get_missing_returns_none() {
        let registry = ToolRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn unregister_removes_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("read_file"));
        let removed = registry.unregister("read_file");
        assert!(removed.is_some());
        assert!(registry.get("read_file").is_none());
    }

    #[test]
    fn unregister_missing_returns_none() {
        let mut registry = ToolRegistry::new();
        assert!(registry.unregister("nonexistent").is_none());
    }

    #[test]
    fn name_collision_overrides() {
        let mut registry = ToolRegistry::new();
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name:        "tool_a".into(),
                description: "version 1".into(),
                parameters:  serde_json::json!({}),
            },
            executor:   Arc::new(|_args, _ctx| Box::pin(async { Ok("v1".into()) })),
            source:     ToolSource::Native,
        });
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name:        "tool_a".into(),
                description: "version 2".into(),
                parameters:  serde_json::json!({}),
            },
            executor:   Arc::new(|_args, _ctx| Box::pin(async { Ok("v2".into()) })),
            source:     ToolSource::Native,
        });

        let tool = registry.get("tool_a").unwrap();
        assert_eq!(tool.definition.description, "version 2");
    }

    #[test]
    fn definitions_returns_all() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("tool_a"));
        registry.register(make_tool("tool_b"));

        let defs = registry.definitions();
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"tool_a"));
        assert!(names.contains(&"tool_b"));
    }

    #[test]
    fn definitions_with_no_policy_returns_all_registered_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("allowed"));
        registry.register(make_tool("denied"));

        let defs = registry.definitions_for_policy(None, ToolExposureMode::AutoApprovedOnly);

        let names: Vec<&str> = defs.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(defs.len(), 2);
        assert!(names.contains(&"allowed"));
        assert!(names.contains(&"denied"));
    }

    #[test]
    fn definitions_for_policy_omits_denied_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("read_file"));
        registry.register(make_tool("write_file"));
        let policy = NamedPolicy::new([
            ("read_file", ToolAccess::Allowed),
            ("write_file", ToolAccess::Denied),
        ]);

        let defs = registry
            .definitions_for_policy(Some(&policy), ToolExposureMode::IncludeRequiresApproval);

        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "read_file");
    }

    #[test]
    fn definitions_for_policy_exposes_approval_tools_only_when_enabled() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("read_file"));
        registry.register(make_tool("shell"));
        let policy = NamedPolicy::new([
            ("read_file", ToolAccess::Allowed),
            ("shell", ToolAccess::RequiresApproval),
        ]);

        let auto_only =
            registry.definitions_for_policy(Some(&policy), ToolExposureMode::AutoApprovedOnly);
        let with_approval = registry
            .definitions_for_policy(Some(&policy), ToolExposureMode::IncludeRequiresApproval);

        assert_eq!(
            auto_only
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["read_file"]
        );
        let with_approval_names: Vec<&str> = with_approval
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(with_approval_names.len(), 2);
        assert!(with_approval_names.contains(&"read_file"));
        assert!(with_approval_names.contains(&"shell"));
    }

    #[test]
    fn names_returns_all() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("tool_x"));
        registry.register(make_tool("tool_y"));

        let names = registry.names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"tool_x".to_string()));
        assert!(names.contains(&"tool_y".to_string()));
    }

    #[tokio::test]
    async fn executor_can_be_called() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("echo"));

        let tool = registry.get("echo").unwrap();

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let ctx = ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        };
        let result = (tool.executor)(serde_json::json!({}), ctx).await;
        assert_eq!(result.unwrap(), "ok");
    }

    #[test]
    fn default_creates_empty_registry() {
        let registry = ToolRegistry::default();
        assert!(registry.names().is_empty());
        assert!(registry.definitions().is_empty());
    }
}
