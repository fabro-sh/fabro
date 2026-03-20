use crate::profiles::EnvContext;
use crate::sandbox::Sandbox;
use crate::skills::Skill;
use crate::subagent::{
    make_close_agent_tool, make_send_input_tool, make_spawn_agent_tool, SessionFactory,
    SubAgentManager,
};
use crate::tool_registry::ToolRegistry;
use fabro_llm::types::ToolDefinition;
use fabro_model::Provider;
use std::sync::Arc;

/// Static capabilities of a provider profile.
pub struct ProfileCapabilities {
    pub supports_reasoning: bool,
    pub supports_streaming: bool,
    pub supports_parallel_tool_calls: bool,
    pub context_window_size: usize,
}

pub trait ProviderProfile: Send + Sync {
    fn provider(&self) -> Provider;
    fn model(&self) -> &str;
    fn tool_registry(&self) -> &ToolRegistry;
    fn tool_registry_mut(&mut self) -> &mut ToolRegistry;
    fn build_system_prompt(
        &self,
        env: &dyn Sandbox,
        env_context: &EnvContext,
        memory: &[String],
        user_instructions: Option<&str>,
        skills: &[Skill],
    ) -> String;
    fn capabilities(&self) -> ProfileCapabilities;
    fn knowledge_cutoff(&self) -> &str;

    fn tools(&self) -> Vec<ToolDefinition> {
        self.tool_registry().definitions()
    }

    fn provider_options(&self) -> Option<serde_json::Value> {
        None
    }

    fn supports_reasoning(&self) -> bool {
        self.capabilities().supports_reasoning
    }

    fn supports_streaming(&self) -> bool {
        self.capabilities().supports_streaming
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.capabilities().supports_parallel_tool_calls
    }

    fn context_window_size(&self) -> usize {
        self.capabilities().context_window_size
    }

    fn register_subagent_tools(
        &mut self,
        manager: Arc<tokio::sync::Mutex<SubAgentManager>>,
        session_factory: SessionFactory,
        current_depth: usize,
    ) {
        self.tool_registry_mut().register(make_spawn_agent_tool(
            manager.clone(),
            session_factory,
            current_depth,
        ));
        self.tool_registry_mut()
            .register(make_send_input_tool(manager.clone()));
        self.tool_registry_mut()
            .register(crate::subagent::make_wait_tool(manager.clone()));
        self.tool_registry_mut()
            .register(make_close_agent_tool(manager));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockSandbox, TestProfile};
    use fabro_model::Provider;

    #[test]
    fn profile_provider_and_model() {
        let profile = TestProfile::new();
        assert_eq!(profile.provider(), Provider::Anthropic);
        assert_eq!(profile.model(), "mock-model");
    }

    #[test]
    fn profile_capabilities() {
        let profile = TestProfile::new();
        assert!(!profile.supports_reasoning());
        assert!(!profile.supports_streaming());
        assert!(!profile.supports_parallel_tool_calls());
        assert_eq!(profile.context_window_size(), 200_000);
    }

    #[test]
    fn profile_build_system_prompt() {
        let profile = TestProfile::new();
        let env = MockSandbox::linux();
        let ctx = EnvContext::default();
        let docs = vec!["README.md contents".into()];
        let prompt = profile.build_system_prompt(&env, &ctx, &docs, None, &[]);
        assert!(prompt.contains("test assistant"));
    }

    #[test]
    fn profile_build_system_prompt_with_user_instructions() {
        let profile = TestProfile::new();
        let env = MockSandbox::default();
        let ctx = EnvContext::default();
        let prompt = profile.build_system_prompt(&env, &ctx, &[], Some("Always use TDD"), &[]);
        assert!(prompt.contains("Always use TDD"));
    }

    #[test]
    fn profile_provider_options_none() {
        let profile = TestProfile::new();
        assert!(profile.provider_options().is_none());
    }

    #[test]
    fn profile_tools_empty_registry() {
        let profile = TestProfile::new();
        assert!(profile.tools().is_empty());
    }
}
