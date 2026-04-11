pub mod anthropic;
pub mod gemini;
pub mod openai;

pub use anthropic::AnthropicProfile;
use fabro_model::Provider;
pub use gemini::GeminiProfile;
pub use openai::OpenAiProfile;

use crate::sandbox::Sandbox;
use crate::skills::{Skill, format_skills_prompt_section};
use crate::tool_registry::ToolRegistry;

/// Common fields shared by all provider profiles.
///
/// Each concrete profile embeds this struct and delegates `provider()`,
/// `model()`, `tool_registry()`, and `tool_registry_mut()` to it.
pub struct BaseProfile {
    pub provider: Provider,
    pub model: String,
    pub registry: ToolRegistry,
}

/// Additional context for building environment blocks
#[derive(Default)]
pub struct EnvContext {
    pub git_branch: Option<String>,
    pub is_git_repo: bool,
    pub current_date: String,
    pub model: String,
    pub knowledge_cutoff: String,
    pub git_status_short: Option<String>,
    pub git_recent_commits: Option<String>,
}

/// Assembles a complete system prompt from a core prompt template and standard
/// sections.
///
/// The `core_prompt` should contain `{env_block}` as a placeholder where the
/// environment context block will be inserted. Project docs and user
/// instructions are appended at the end.
#[must_use]
pub fn assemble_system_prompt(
    core_prompt: &str,
    env: &dyn Sandbox,
    env_context: &EnvContext,
    memory: &[String],
    user_instructions: Option<&str>,
    skills: &[Skill],
) -> String {
    let env_block = build_env_context_block_with(env, env_context);
    let docs_section = if memory.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", memory.join("\n\n"))
    };
    let skills_section = {
        let s = format_skills_prompt_section(skills);
        if s.is_empty() {
            String::new()
        } else {
            format!("\n\n{s}")
        }
    };
    let user_section = match user_instructions {
        Some(instructions) => format!("\n\n# User Instructions\n{instructions}"),
        None => String::new(),
    };

    let prompt = core_prompt.replace("{env_block}", &env_block);
    format!("{prompt}{docs_section}{skills_section}{user_section}")
}

#[cfg(test)]
#[must_use]
pub fn build_env_context_block(env: &dyn Sandbox) -> String {
    build_env_context_block_with(env, &EnvContext::default())
}

#[must_use]
pub fn build_env_context_block_with(env: &dyn Sandbox, ctx: &EnvContext) -> String {
    let mut lines = vec![
        "<environment>".to_string(),
        format!("Working directory: {}", env.working_directory()),
        format!("Is git repository: {}", ctx.is_git_repo),
    ];

    if let Some(ref branch) = ctx.git_branch {
        lines.push(format!("Git branch: {branch}"));
    }

    lines.push(format!("Platform: {}", env.platform()));
    lines.push(format!("OS version: {}", env.os_version()));

    if !ctx.current_date.is_empty() {
        lines.push(format!("Today's date: {}", ctx.current_date));
    }
    if !ctx.model.is_empty() {
        lines.push(format!("Model: {}", ctx.model));
    }
    if !ctx.knowledge_cutoff.is_empty() {
        lines.push(format!("Knowledge cutoff: {}", ctx.knowledge_cutoff));
    }

    if let Some(ref status) = ctx.git_status_short {
        lines.push(format!("Git status:\n{status}"));
    }
    if let Some(ref commits) = ctx.git_recent_commits {
        lines.push(format!("Recent commits:\n{commits}"));
    }

    lines.push("</environment>".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockSandbox;

    #[test]
    fn env_context_block_contains_platform() {
        let env = MockSandbox::linux();
        let block = build_env_context_block(&env);
        assert!(block.contains("<environment>"));
        assert!(block.contains("</environment>"));
        assert!(block.contains("linux"));
        assert!(block.contains("/home/test"));
        assert!(block.contains("Linux 6.1.0"));
    }

    #[test]
    fn env_context_block_with_extra_context() {
        let env = MockSandbox::linux();
        let ctx = EnvContext {
            git_branch: Some("main".into()),
            is_git_repo: true,
            current_date: "2026-02-20".into(),
            model: "claude-opus-4-6".into(),
            knowledge_cutoff: "May 2025".into(),
            git_status_short: None,
            git_recent_commits: None,
        };
        let block = build_env_context_block_with(&env, &ctx);
        assert!(block.contains("Git branch: main"));
        assert!(block.contains("Is git repository: true"));
        assert!(block.contains("Today's date: 2026-02-20"));
        assert!(block.contains("Model: claude-opus-4-6"));
        assert!(block.contains("Knowledge cutoff: May 2025"));
    }
}
