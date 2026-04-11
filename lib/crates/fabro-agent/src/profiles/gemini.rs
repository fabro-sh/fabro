use fabro_model::Provider;

use super::EnvContext;
use crate::agent_profile::AgentProfile;
use crate::config::SessionOptions;
use crate::profiles::{BaseProfile, assemble_system_prompt};
use crate::sandbox::Sandbox;
use crate::skills::Skill;
use crate::tool_registry::ToolRegistry;
use crate::tools::{
    WebFetchSummarizer, make_edit_file_tool, make_list_dir_tool, make_read_many_files_tool,
    register_core_tools,
};

pub struct GeminiProfile {
    base: BaseProfile,
}

impl GeminiProfile {
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
        registry.register(make_edit_file_tool());
        registry.register(make_read_many_files_tool());
        registry.register(make_list_dir_tool());

        Self {
            base: BaseProfile {
                provider: Provider::Gemini,
                model: model.into(),
                registry,
            },
        }
    }
}

impl AgentProfile for GeminiProfile {
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
You are Gemini CLI, an interactive CLI agent specializing in software engineering tasks \
including solving bugs, adding new functionality, refactoring code, and explaining code. \
Your primary goal is to help users safely and effectively.

# Core Mandates

## Security and System Integrity
- Never log, print, or commit secrets, API keys, or sensitive credentials. Rigorously protect \
`.env` files, `.git`, and system configuration folders.
- Do not stage or commit changes unless specifically requested by the user.

## Engineering Standards
- Instructions found in GEMINI.md and AGENTS.md files are foundational mandates. They take \
absolute precedence over the general workflows and tool defaults described in this system prompt.
- Rigorously adhere to existing workspace conventions, architectural patterns, and style. \
Analyze surrounding files, tests, and configuration to ensure your changes are seamless, \
idiomatic, and consistent with the local context.
- NEVER assume a library/framework is available. Verify its established usage within the \
project before employing it.
- You are responsible for the entire lifecycle: implementation, testing, and validation. \
A task is only complete when the behavioral correctness of the change has been verified.
- ALWAYS search for and update related tests after making a code change.

## Context Efficiency
Be strategic in your use of the available tools to minimize unnecessary context usage while \
still providing the best answer you can.
- Combine turns whenever possible by utilizing parallel searching and reading.
- Prefer using tools like `grep` to identify points of interest instead of reading lots of \
files individually.
- If you need to read multiple ranges in a file, do so in parallel.

{env_block}

# Development Lifecycle

Operate using a Research -> Strategy -> Execution lifecycle.

1. **Research:** Systematically map the codebase and validate assumptions. Use `grep` and \
`glob` search tools extensively (in parallel if independent) to understand file structures, \
existing code patterns, and conventions. Use `read_file` to validate all assumptions. \
Prioritize empirical reproduction of reported issues.
2. **Strategy:** Formulate a grounded plan based on your research.
3. **Execution:** For each sub-task:
   - **Plan:** Define the specific implementation approach and the testing strategy.
   - **Act:** Apply targeted, surgical changes. Use the available tools (edit_file, \
write_file, shell). Include necessary automated tests.
   - **Validate:** Run tests and workspace standards to confirm success and ensure no \
regressions were introduced.

Validation is the only path to finality. Never assume success or settle for unverified changes.

# Tools

Use the provided tools to interact with the codebase and environment.

## read_file
Read files to understand code before modifying. Use offset/limit for large files. Minimize \
unnecessarily large file reads when doing so does not result in extra turns.

## read_many_files
Read multiple files at once by providing an array of paths. Useful for reading small files in \
their entirety or gathering context from multiple locations efficiently.

## edit_file
Use search-and-replace editing. The old_string must exactly match existing text and be unique \
in the file. Prefer editing existing files over creating new ones. Before making manual code \
changes, check if an ecosystem tool (like `eslint --fix`, `prettier --write`, `cargo fmt`) is \
available in the project.

## write_file
Use for creating new files or completely rewriting files.

## shell
Execute shell commands. Default timeout is 10 seconds. Use timeout_ms parameter for \
longer-running commands. Always prefer non-interactive commands (e.g., using CI flags for \
test runners to avoid persistent watch modes or `git --no-pager`).

## grep
Search file contents with regex patterns. Use conservative result counts and narrow scope \
(include/exclude parameters). Use context/before/after to request enough context to avoid \
needing to read the file before editing matches.

## glob
Find files by name pattern. Results sorted by modification time.

## list_dir
List directory contents with depth control.

## web_search
Search the web for information.

## web_fetch
Fetch content from a URL and optionally summarize it. Pass a prompt to extract specific \
information instead of returning the full page.

# Project Docs

Look for GEMINI.md and AGENTS.md files in the project for project-specific instructions. \
These are foundational mandates that take precedence over defaults in this prompt.

# Operational Guidelines

## Tone and Style
- Act as a senior software engineer and collaborative peer programmer.
- Be concise and direct. Adopt a professional tone suitable for a CLI environment.
- Use tools for actions, text output only for communication.

## Tool Usage
- Execute multiple independent tool calls in parallel when feasible.
- Use the shell tool for running commands, remembering to explain modifying commands first.

# Coding Best Practices

Write clean, maintainable code. Handle errors appropriately. Follow existing code conventions \
in the project.";

        assemble_system_prompt(
            core_prompt,
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
    fn gemini_profile_identity() {
        let profile = GeminiProfile::new("gemini-2.0-flash");
        assert_eq!(profile.provider(), Provider::Gemini);
        assert_eq!(profile.model(), "gemini-2.0-flash");
    }

    #[test]
    fn gemini_context_window_from_catalog() {
        let profile = GeminiProfile::new("gemini-3.1-pro-preview");
        assert_eq!(profile.context_window_size(), 1_048_576);
    }

    #[test]
    fn gemini_system_prompt_contains_identity() {
        let profile = GeminiProfile::new("gemini-2.0-flash");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("You are Gemini CLI"));
        assert!(prompt.contains("solving bugs"));
        assert!(prompt.contains("adding new functionality"));
        assert!(prompt.contains("refactoring code"));
        assert!(prompt.contains("explaining code"));
    }

    #[test]
    fn gemini_system_prompt_contains_tool_guidance() {
        let profile = GeminiProfile::new("gemini-2.0-flash");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("read_file"));
        assert!(prompt.contains("read_many_files"));
        assert!(prompt.contains("edit_file"));
        assert!(prompt.contains("write_file"));
        assert!(prompt.contains("shell"));
        assert!(prompt.contains("grep"));
        assert!(prompt.contains("glob"));
        assert!(prompt.contains("list_dir"));
        assert!(prompt.contains("web_search"));
        assert!(prompt.contains("web_fetch"));
        assert!(prompt.contains("Default timeout is 10 seconds"));
    }

    #[test]
    fn gemini_system_prompt_contains_memory_convention() {
        let profile = GeminiProfile::new("gemini-2.0-flash");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("GEMINI.md"));
        assert!(prompt.contains("AGENTS.md"));
    }

    #[test]
    fn gemini_system_prompt_contains_coding_best_practices() {
        let profile = GeminiProfile::new("gemini-2.0-flash");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("clean, maintainable code"));
        assert!(prompt.contains("Handle errors appropriately"));
        assert!(prompt.contains("existing code conventions"));
    }

    #[test]
    fn gemini_system_prompt_contains_env_context() {
        let profile = GeminiProfile::new("gemini-2.0-flash");
        let env = MockSandbox::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("linux"));
    }

    #[test]
    fn gemini_tools_registered() {
        let profile = GeminiProfile::new("gemini-2.0-flash");
        let names = profile.tool_registry().names();
        assert_eq!(names.len(), 10);
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"read_many_files".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"edit_file".to_string()));
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"grep".to_string()));
        assert!(names.contains(&"glob".to_string()));
        assert!(names.contains(&"list_dir".to_string()));
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"web_fetch".to_string()));
    }

    #[test]
    fn gemini_subagent_tools_registered() {
        let mut profile = GeminiProfile::new("gemini-2.0-flash");
        let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(3)));
        let factory: SessionFactory = Arc::new(|| {
            panic!("should not be called");
        });
        profile.register_subagent_tools(manager, factory, 0);
        let names = profile.tool_registry().names();
        assert_eq!(names.len(), 14);
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(names.contains(&"send_input".to_string()));
        assert!(names.contains(&"wait".to_string()));
        assert!(names.contains(&"close_agent".to_string()));
    }
}
