use crate::config::SessionConfig;
use crate::profiles::assemble_system_prompt;
use crate::profiles::BaseProfile;
use crate::provider_profile::{ProfileCapabilities, ProviderProfile};
use crate::sandbox::Sandbox;
use crate::skills::Skill;
use crate::tool_registry::ToolRegistry;
use crate::tools::{make_edit_file_tool, register_core_tools, WebFetchSummarizer};
use fabro_model::Provider;

use super::EnvContext;

pub struct AnthropicProfile {
    base: BaseProfile,
}

impl AnthropicProfile {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self::with_summarizer(model, None)
    }

    #[must_use]
    pub fn with_summarizer(
        model: impl Into<String>,
        summarizer: Option<WebFetchSummarizer>,
    ) -> Self {
        let config = SessionConfig {
            default_command_timeout_ms: 120_000,
            ..SessionConfig::default()
        };
        let mut registry = ToolRegistry::new();

        register_core_tools(&mut registry, &config, summarizer);
        registry.register(make_edit_file_tool());

        Self {
            base: BaseProfile {
                provider: Provider::Anthropic,
                model: model.into(),
                registry,
            },
        }
    }

    /// Override the provider identity (e.g. for Kimi, which uses the Anthropic
    /// Messages protocol but routes to a different adapter).
    #[must_use]
    pub fn with_provider(mut self, provider: Provider) -> Self {
        self.base.provider = provider;
        self
    }
}

impl ProviderProfile for AnthropicProfile {
    fn provider(&self) -> Provider {
        self.base.provider
    }

    fn model(&self) -> &str {
        &self.base.model
    }

    fn tool_registry(&self) -> &ToolRegistry {
        &self.base.registry
    }

    fn tool_registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.base.registry
    }

    fn build_system_prompt(
        &self,
        env: &dyn Sandbox,
        env_context: &EnvContext,
        memory: &[String],
        user_instructions: Option<&str>,
        skills: &[Skill],
    ) -> String {
        let core_prompt = "\
You are Claude, an AI coding assistant made by Anthropic. You help users with software \
engineering tasks including solving bugs, adding new functionality, refactoring code, \
explaining code, and more.

You are an interactive agent that helps users with software engineering tasks. Use the \
instructions below and the tools available to you to assist the user.

{env_block}

# Doing Tasks

- The user will primarily request you to perform software engineering tasks. These may include \
solving bugs, adding new functionality, refactoring code, explaining code, and more.
- In general, do not propose changes to code you have not read. If a user asks about or wants \
you to modify a file, read it first. Understand existing code before suggesting modifications.
- Do not create files unless they are absolutely necessary for achieving your goal. Generally \
prefer editing an existing file to creating a new one, as this prevents file bloat and builds \
on existing work more effectively.
- If your approach is blocked, do not attempt to brute force your way to the outcome. Consider \
alternative approaches or other ways you might unblock yourself.
- Avoid over-engineering. Only make changes that are directly requested or clearly necessary. \
Keep solutions simple and focused.
- Do not add features, refactor code, or make improvements beyond what was asked.
- Do not add error handling, fallbacks, or validation for scenarios that cannot happen. Trust \
internal code and framework guarantees. Only validate at system boundaries (user input, external APIs).
- Avoid backwards-compatibility hacks. If you are certain something is unused, delete it completely.

# Tools

Use the provided tools to interact with the codebase and environment. Do NOT use the shell \
tool to run commands when a relevant dedicated tool is provided:
- To read files use read_file instead of cat, head, tail, or sed.
- To edit files use edit_file instead of sed or awk.
- To create files use write_file instead of cat with heredoc or echo redirection.
- To search for files use glob instead of find or ls.
- To search the content of files use grep instead of grep or rg.

## read_file
Read files before editing them. Always read a file before attempting to edit it. Use \
offset/limit for large files. Reading a file you have not read before is always appropriate.

## edit_file
Performs exact string replacements in files. The old_string must be an exact match of \
existing text and must be unique in the file. If old_string matches multiple locations, provide \
more surrounding context to make it unique. Prefer editing existing files over creating new ones. \
When editing text, ensure you preserve the exact indentation as it appears in the file.

## write_file
Use write_file only when creating new files. Prefer edit_file for modifying existing files. \
Always prefer editing existing files in the codebase over creating new ones.

## shell
Use for running commands, tests, and builds. Default timeout is 120 seconds. Use timeout_ms \
parameter for longer-running commands.

## grep
Search file contents with regex patterns. Supports output modes: content, files_with_matches, count. \
Use this for searching the content of files rather than using shell grep or rg.

## glob
Find files by name pattern. Results sorted by modification time (newest first). Use this for \
finding files rather than using shell find or ls commands.

## web_search
Search the web using Brave Search. Returns titles, URLs, and descriptions.

## web_fetch
Fetch content from a URL and optionally summarize it. Pass a prompt to extract specific \
information instead of returning the full page. URLs must start with http:// or https://.

# Coding Best Practices

Write clean, maintainable code. Handle errors appropriately. Follow existing code conventions \
in the project. Keep changes minimal and focused on the task.";

        assemble_system_prompt(
            core_prompt,
            env,
            env_context,
            memory,
            user_instructions,
            skills,
        )
    }

    fn capabilities(&self) -> ProfileCapabilities {
        let context_window_size = fabro_model::get_model_info(self.model())
            .map(|info| info.limits.context_window as usize)
            .unwrap_or_else(|| {
                if self.model().contains("opus-4-6") {
                    1_000_000
                } else {
                    200_000
                }
            });
        ProfileCapabilities {
            supports_reasoning: true,
            supports_streaming: true,
            supports_parallel_tool_calls: true,
            context_window_size,
        }
    }

    fn provider_options(&self) -> Option<serde_json::Value> {
        let model = self.model();
        if model.contains("opus-4-6") {
            Some(serde_json::json!({
                "anthropic": {
                    "thinking": {"type": "adaptive"},
                    "beta_headers": ["context-1m-2025-08-07"]
                }
            }))
        } else if model.contains("sonnet-4-6") {
            Some(serde_json::json!({
                "anthropic": {
                    "thinking": {"type": "adaptive"}
                }
            }))
        } else {
            None
        }
    }

    fn knowledge_cutoff(&self) -> &'static str {
        "May 2025"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockSandbox;

    #[test]
    fn anthropic_profile_identity() {
        let profile = AnthropicProfile::new("claude-sonnet-4-20250514");
        assert_eq!(profile.provider(), Provider::Anthropic);
        assert_eq!(profile.model(), "claude-sonnet-4-20250514");
    }

    #[test]
    fn anthropic_profile_capabilities() {
        let profile = AnthropicProfile::new("claude-sonnet-4-20250514");
        assert!(profile.supports_reasoning());
        assert!(profile.supports_streaming());
        assert!(profile.supports_parallel_tool_calls());
        assert_eq!(profile.context_window_size(), 200_000);
    }

    #[test]
    fn anthropic_opus_4_6_has_1m_context_window() {
        let profile = AnthropicProfile::new("claude-opus-4-6");
        assert_eq!(profile.context_window_size(), 1_000_000);
    }

    #[test]
    fn anthropic_system_prompt_contains_env_context() {
        let profile = AnthropicProfile::new("claude-sonnet-4-20250514");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("You are Claude, an AI coding assistant made by Anthropic"));
        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("linux"));
        assert!(prompt.contains("/home/test"));
        assert!(prompt.contains("# Tools"));
        // Verify expanded tool guidance
        assert!(
            prompt.contains("old_string must be"),
            "prompt should contain edit_file guidance about old_string"
        );
        assert!(
            prompt.contains("exact match"),
            "prompt should contain edit_file guidance about exact match"
        );
        assert!(
            prompt.contains("Read files before editing"),
            "prompt should contain read_file guidance"
        );
        assert!(
            prompt.contains("Default timeout is 120 seconds"),
            "prompt should contain shell timeout guidance"
        );
        assert!(
            prompt.contains("Write clean, maintainable code"),
            "prompt should contain coding best practices"
        );
        assert!(
            prompt.contains("web_search"),
            "prompt should contain web_search guidance"
        );
        assert!(
            prompt.contains("web_fetch"),
            "prompt should contain web_fetch guidance"
        );
    }

    #[test]
    fn anthropic_system_prompt_includes_memory() {
        let profile = AnthropicProfile::new("claude-sonnet-4-20250514");
        let env = MockSandbox::linux();
        let docs = vec!["# Project README".into(), "# CONTRIBUTING guide".into()];
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &docs, None, &[]);
        assert!(prompt.contains("# Project README"));
        assert!(prompt.contains("# CONTRIBUTING guide"));
    }

    #[test]
    fn anthropic_system_prompt_includes_env_context() {
        let profile = AnthropicProfile::new("claude-opus-4-6");
        let env = MockSandbox::linux();
        let ctx = EnvContext {
            git_branch: Some("feature-branch".into()),
            is_git_repo: true,
            current_date: "2026-02-20".into(),
            model: "claude-opus-4-6".into(),
            knowledge_cutoff: "May 2025".into(),
            git_status_short: None,
            git_recent_commits: None,
        };
        let prompt = profile.build_system_prompt(&env, &ctx, &[], None, &[]);
        assert!(prompt.contains("Git branch: feature-branch"));
        assert!(prompt.contains("Is git repository: true"));
        assert!(prompt.contains("Today's date: 2026-02-20"));
        assert!(prompt.contains("Model: claude-opus-4-6"));
        assert!(prompt.contains("Knowledge cutoff: May 2025"));
    }

    #[test]
    fn anthropic_system_prompt_includes_user_instructions() {
        let profile = AnthropicProfile::new("claude-opus-4-6");
        let env = MockSandbox::linux();
        let ctx = EnvContext::default();
        let prompt =
            profile.build_system_prompt(&env, &ctx, &[], Some("Always write tests first"), &[]);
        assert!(prompt.contains("Always write tests first"));
        assert!(prompt.contains("# User Instructions"));
    }

    #[test]
    fn anthropic_tools_registered() {
        let profile = AnthropicProfile::new("claude-sonnet-4-20250514");
        let names = profile.tool_registry().names();
        assert_eq!(names.len(), 8);
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"edit_file".to_string()));
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"grep".to_string()));
        assert!(names.contains(&"glob".to_string()));
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"web_fetch".to_string()));
    }

    #[test]
    fn anthropic_provider_options_include_thinking_for_opus_4_6() {
        let profile = AnthropicProfile::new("claude-opus-4-6");
        let options = profile.provider_options();
        assert!(options.is_some(), "provider_options should return Some");
        let options = options.unwrap();
        let thinking_type = options["anthropic"]["thinking"]["type"].as_str();
        assert_eq!(
            thinking_type,
            Some("adaptive"),
            "thinking type should be adaptive"
        );
        let beta_headers = options["anthropic"]["beta_headers"]
            .as_array()
            .expect("beta_headers should be an array");
        assert_eq!(beta_headers[0].as_str(), Some("context-1m-2025-08-07"));
    }

    #[test]
    fn anthropic_provider_options_include_thinking_for_sonnet_4_6() {
        let profile = AnthropicProfile::new("claude-sonnet-4-6");
        let options = profile.provider_options();
        assert!(options.is_some(), "provider_options should return Some");
        let options = options.unwrap();
        let thinking_type = options["anthropic"]["thinking"]["type"].as_str();
        assert_eq!(
            thinking_type,
            Some("adaptive"),
            "thinking type should be adaptive"
        );
    }

    #[test]
    fn anthropic_provider_options_none_for_older_models() {
        let profile = AnthropicProfile::new("claude-sonnet-4-5");
        assert!(
            profile.provider_options().is_none(),
            "older models should not have provider_options"
        );
    }

    #[test]
    fn anthropic_register_subagent_tools() {
        use crate::subagent::{SessionFactory, SubAgentManager};
        use std::sync::Arc;

        let mut profile = AnthropicProfile::new("claude-sonnet-4-20250514");
        assert_eq!(profile.tool_registry().names().len(), 8);

        let manager = Arc::new(tokio::sync::Mutex::new(SubAgentManager::new(3)));
        let factory: SessionFactory = Arc::new(|| {
            panic!("should not be called in test");
        });

        profile.register_subagent_tools(manager, factory, 0);

        let names = profile.tool_registry().names();
        assert_eq!(names.len(), 12, "should have 8 base + 4 subagent tools");
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(names.contains(&"send_input".to_string()));
        assert!(names.contains(&"wait".to_string()));
        assert!(names.contains(&"close_agent".to_string()));
    }
}
