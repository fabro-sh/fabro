use fabro_model::Provider;

use super::EnvContext;
use crate::agent_profile::AgentProfile;
use crate::config::SessionOptions;
use crate::profiles::{BaseProfile, assemble_system_prompt};
use crate::sandbox::Sandbox;
use crate::skills::Skill;
use crate::tool_registry::ToolRegistry;
use crate::tools::{WebFetchSummarizer, register_core_tools};
use crate::v4a_patch::make_apply_patch_tool;

pub struct OpenAiProfile {
    base: BaseProfile,
}

impl OpenAiProfile {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self::with_summarizer(model, None)
    }

    #[must_use]
    pub fn with_summarizer(
        model: impl Into<String>,
        summarizer: Option<WebFetchSummarizer>,
    ) -> Self {
        let config = SessionOptions::default();
        let mut registry = ToolRegistry::new();

        register_core_tools(&mut registry, &config, summarizer);
        registry.register(make_apply_patch_tool());

        Self {
            base: BaseProfile {
                provider: Provider::OpenAi,
                model: model.into(),
                registry,
            },
        }
    }

    /// Override the provider identity (e.g. for Z.AI or Minimax, which use the
    /// OpenAI Chat Completions protocol but route to different adapters).
    #[must_use]
    pub fn with_provider(mut self, provider: Provider) -> Self {
        self.base.provider = provider;
        self
    }

    fn provider_display_name(&self) -> &str {
        match self.base.provider {
            Provider::OpenAi => "OpenAI",
            Provider::Kimi => "Moonshot",
            Provider::Zai => "Zhipu AI",
            Provider::Minimax => "MiniMax",
            Provider::Inception => "Inception",
            other => <&'static str>::from(other),
        }
    }
}

impl AgentProfile for OpenAiProfile {
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
        let provider_name = self.provider_display_name();
        let core_prompt = format!("\
You are a coding agent powered by {provider_name}, running in a terminal-based agentic coding assistant. \
You are expected to be precise, safe, and helpful.

You can receive user prompts and context such as files in the workspace, communicate with the \
user by streaming thinking and responses, and emit function calls to run terminal commands and \
apply patches.

# Personality

Be concise, direct, and friendly. Communicate efficiently, keeping the user clearly informed \
about ongoing actions without unnecessary detail. Prioritize actionable guidance, clearly \
stating assumptions, environment prerequisites, and next steps.

{{env_block}}

# AGENTS.md

Repos may contain AGENTS.md files with instructions for the agent. These files can appear \
anywhere in the repository. Instructions in AGENTS.md files whose scope includes a file you \
touch must be obeyed. More-deeply-nested AGENTS.md files take precedence in case of conflict. \
Direct system/developer/user instructions take precedence over AGENTS.md instructions.

# Task Execution

Keep going until the task is completely resolved before ending your turn. Autonomously resolve \
the query to the best of your ability using the tools available. Do NOT guess or make up an answer.

Working on repos in the current environment is allowed, even if they are proprietary.

If completing the task requires writing or modifying files:
- Fix the problem at the root cause rather than applying surface-level patches, when possible.
- Avoid unneeded complexity in your solution.
- Do not attempt to fix unrelated bugs or broken tests.
- Keep changes consistent with the style of the existing codebase. Changes should be minimal \
and focused on the task.
- Use `git log` and `git blame` to search the history of the codebase if additional context is needed.
- NEVER add copyright or license headers unless specifically requested.
- When apply_patch fails, the error includes the current file contents — use them to construct \
a corrected patch without re-reading the file.
- Do not `git commit` your changes or create new git branches unless explicitly requested.

# Validating Your Work

If the codebase has tests or the ability to build or run, consider using them to verify your \
work. Start as specific as possible to the code you changed to catch issues efficiently, then \
make your way to broader tests as you build confidence.

# Tools

Use the provided tools to interact with the codebase and environment.

## read_file
Read files to understand code before modifying. Use offset/limit for large files.

## apply_patch
Use the v4a patch format for all file modifications. The format uses `*** Begin Patch` / \
`*** End Patch` delimiters with `*** Add File:`, `*** Delete File:`, `*** Update File:` \
operations. Update hunks use `@@ context line text` headers — place a line of \
existing code after `@@ ` to anchor each hunk. Use `-` for \
removals, `+` for additions, and space-prefix for unchanged context lines. Show 3 lines \
of context around each change. NEVER use `applypatch` or `apply-patch`, only `apply_patch`.

Example:
```
*** Begin Patch
*** Update File: src/main.py
@@ def hello():
-    print(\"old\")
+    print(\"new\")
*** End Patch
```

## write_file
Use for creating new files. For modifications, prefer apply_patch.

## shell
Execute shell commands. Default timeout is 10 seconds. Use timeout_ms parameter for \
longer-running commands. When searching for text or files, prefer `rg` (ripgrep) because \
it is much faster than alternatives like `grep`.

## grep
Search file contents with regex. Use glob_filter to narrow results.

## glob
Find files by name pattern.

## web_search
Search the web using Brave Search. Returns titles, URLs, and descriptions.

## web_fetch
Fetch content from a URL and optionally summarize it. Pass a prompt to extract specific \
information instead of returning the full page. URLs must start with http:// or https://.

# Coding Best Practices

Write clean, maintainable code. Handle errors appropriately. Follow existing code conventions \
in the project.");

        assemble_system_prompt(
            &core_prompt,
            env,
            env_context,
            memory,
            user_instructions,
            skills,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::subagent::{SessionFactory, SubAgentManager};
    use crate::test_support::MockSandbox;

    #[test]
    fn openai_profile_identity() {
        let profile = OpenAiProfile::new("o3-mini");
        assert_eq!(profile.provider(), Provider::OpenAi);
        assert_eq!(profile.model(), "o3-mini");
    }

    #[test]
    fn openai_system_prompt_contains_env_context() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("You are a coding agent powered by OpenAI"));
        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("linux"));
        assert!(prompt.contains("v4a patch format"));
        assert!(prompt.contains("*** Begin Patch"));
    }

    #[test]
    fn openai_system_prompt_contains_tool_guidance() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("read_file"));
        assert!(prompt.contains("apply_patch"));
        assert!(prompt.contains("write_file"));
        assert!(prompt.contains("shell"));
        assert!(prompt.contains("grep"));
        assert!(prompt.contains("glob"));
        assert!(prompt.contains("timeout_ms"));
    }

    #[test]
    fn openai_system_prompt_contains_coding_best_practices() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("clean, maintainable code"));
        assert!(prompt.contains("existing code conventions"));
    }

    #[test]
    fn openai_system_prompt_includes_memory() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockSandbox::linux();
        let docs = vec!["# Project README".into(), "# CONTRIBUTING guide".into()];
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &docs, None, &[]);
        assert!(prompt.contains("# Project README"));
        assert!(prompt.contains("# CONTRIBUTING guide"));
    }

    #[test]
    fn openai_system_prompt_includes_user_instructions() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(
            &env,
            &EnvContext::default(),
            &[],
            Some("Always write tests first"),
            &[],
        );
        assert!(prompt.contains("Always write tests first"));
        assert!(prompt.contains("# User Instructions"));
    }

    #[test]
    fn openai_subagent_tools_registered() {
        let mut profile = OpenAiProfile::new("o3-mini");
        assert_eq!(profile.tool_registry().names().len(), 8);

        let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(3)));
        let factory: SessionFactory = Arc::new(|| panic!("should not be called in test"));
        profile.register_subagent_tools(manager, factory, 0);
        assert_eq!(profile.tool_registry().names().len(), 12);
    }

    #[test]
    fn openai_tools_registered() {
        let profile = OpenAiProfile::new("o3-mini");
        let names = profile.tool_registry().names();
        assert_eq!(names.len(), 8);
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"grep".to_string()));
        assert!(names.contains(&"glob".to_string()));
        assert!(names.contains(&"apply_patch".to_string()));
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"web_fetch".to_string()));
    }

    #[test]
    fn kimi_provider_prompt_says_moonshot() {
        let profile = OpenAiProfile::new("kimi-k2.5").with_provider(Provider::Kimi);
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("powered by Moonshot"));
        assert!(!prompt.contains("powered by OpenAI"));
    }

    #[test]
    fn zai_provider_prompt_says_zhipu() {
        let profile = OpenAiProfile::new("glm-4.7").with_provider(Provider::Zai);
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("powered by Zhipu AI"));
    }

    #[test]
    fn minimax_provider_prompt_says_minimax() {
        let profile = OpenAiProfile::new("minimax-m2.5").with_provider(Provider::Minimax);
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("powered by MiniMax"));
    }

    #[test]
    fn inception_provider_prompt_says_inception() {
        let profile = OpenAiProfile::new("mercury-2").with_provider(Provider::Inception);
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("powered by Inception"));
    }
}
