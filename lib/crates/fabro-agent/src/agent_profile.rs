use std::sync::Arc;

use fabro_llm::types::ToolDefinition;
use fabro_model::{Catalog, Provider};
use tokio::sync::Mutex;

use crate::profiles::EnvContext;
use crate::sandbox::Sandbox;
use crate::skills::Skill;
use crate::subagent::{
    SessionFactory, SubAgentManager, make_close_agent_tool, make_send_input_tool,
    make_spawn_agent_tool, make_wait_tool,
};
use crate::tool_registry::ToolRegistry;

pub trait AgentProfile: Send + Sync {
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

    fn tools(&self) -> Vec<ToolDefinition> {
        self.tool_registry().definitions()
    }

    fn knowledge_cutoff(&self) -> Option<String> {
        Catalog::builtin()
            .get(self.model())
            .and_then(|m| m.knowledge_cutoff().map(str::to_string))
    }

    fn context_window_size(&self) -> usize {
        Catalog::builtin()
            .get(self.model())
            .map_or(200_000, |m| usize::try_from(m.context_window()).unwrap())
    }

    fn register_subagent_tools(
        &mut self,
        manager: Arc<Mutex<SubAgentManager>>,
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
            .register(make_wait_tool(manager.clone()));
        self.tool_registry_mut()
            .register(make_close_agent_tool(manager));
    }
}

#[cfg(test)]
mod tests {
    use fabro_model::Provider;

    use super::*;
    use crate::test_support::{MockSandbox, TestProfile};

    #[test]
    fn profile_provider_and_model() {
        let profile = TestProfile::new();
        assert_eq!(profile.provider(), Provider::Anthropic);
        assert_eq!(profile.model(), "mock-model");
    }

    #[test]
    fn profile_context_window_defaults() {
        let profile = TestProfile::new();
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
    fn profile_tools_empty_registry() {
        let profile = TestProfile::new();
        assert!(profile.tools().is_empty());
    }
}
