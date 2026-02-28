use crate::execution_env::ExecutionEnvironment;
use crate::profiles::assemble_system_prompt;
use crate::profiles::BaseProfile;
use crate::provider_profile::{ProfileCapabilities, ProviderProfile};
use crate::skills::Skill;
use crate::tool_registry::{RegisteredTool, ToolRegistry};
use llm::types::ToolDefinition;
use crate::tools::{
    make_glob_tool, make_grep_tool, make_read_file_tool, make_shell_tool, make_web_fetch_tool,
    make_web_search_tool, make_write_file_tool,
};
use std::sync::Arc;

use super::EnvContext;

pub struct OpenAiProfile {
    base: BaseProfile,
    reasoning_effort: Option<String>,
}

impl OpenAiProfile {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        let mut registry = ToolRegistry::new();

        registry.register(make_read_file_tool());
        registry.register(make_write_file_tool());
        registry.register(make_shell_tool());
        registry.register(make_grep_tool());
        registry.register(make_glob_tool());
        registry.register(make_apply_patch_tool());
        registry.register(make_web_search_tool());
        registry.register(make_web_fetch_tool());

        Self {
            base: BaseProfile {
                id: "openai",
                model: model.into(),
                registry,
            },
            reasoning_effort: None,
        }
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }
}

impl ProviderProfile for OpenAiProfile {
    fn id(&self) -> &str {
        self.base.id
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
        env: &dyn ExecutionEnvironment,
        env_context: &EnvContext,
        project_docs: &[String],
        user_instructions: Option<&str>,
        skills: &[Skill],
    ) -> String {
        let core_prompt = "\
You are a coding agent powered by OpenAI, running in a terminal-based agentic coding assistant. \
You are expected to be precise, safe, and helpful.

You can receive user prompts and context such as files in the workspace, communicate with the \
user by streaming thinking and responses, and emit function calls to run terminal commands and \
apply patches.

# Personality

Be concise, direct, and friendly. Communicate efficiently, keeping the user clearly informed \
about ongoing actions without unnecessary detail. Prioritize actionable guidance, clearly \
stating assumptions, environment prerequisites, and next steps.

{env_block}

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
- Do not waste tokens re-reading files after calling apply_patch on them. The tool call will \
fail if it did not work.
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
operations. Update operations use `@@` context hints and +/- prefixes for changes. Show 3 \
lines of context around each change. NEVER use `applypatch` or `apply-patch`, only `apply_patch`.

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
Fetch content from a URL. URLs must start with http:// or https://.

# Coding Best Practices

Write clean, maintainable code. Handle errors appropriately. Follow existing code conventions \
in the project.";

        assemble_system_prompt(core_prompt, env, env_context, project_docs, user_instructions, skills)
    }

    fn capabilities(&self) -> ProfileCapabilities {
        ProfileCapabilities {
            supports_reasoning: true,
            supports_streaming: true,
            supports_parallel_tool_calls: true,
            context_window_size: 128_000,
        }
    }

    fn provider_options(&self) -> Option<serde_json::Value> {
        self.reasoning_effort.as_ref().map(|effort| {
            serde_json::json!({
                "openai": {
                    "reasoning": {
                        "effort": effort
                    }
                }
            })
        })
    }

    fn knowledge_cutoff(&self) -> &'static str {
        "April 2025"
    }
}

// -- apply_patch v4a format --

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Remove(String),
    Add(String),
    Context(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub context_line: String,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOperation {
    Add { path: String, content: String },
    Delete { path: String },
    Update { path: String, hunks: Vec<Hunk> },
}

/// Parses a v4a format patch string into a list of patch operations.
///
/// # Errors
/// Returns an error if the patch format is invalid.
pub fn parse_v4a_patch(text: &str) -> Result<Vec<PatchOperation>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut ops = Vec::new();
    let mut i = 0;

    // Find "*** Begin Patch"
    while i < lines.len() {
        if lines[i].trim() == "*** Begin Patch" {
            i += 1;
            break;
        }
        i += 1;
    }

    while i < lines.len() {
        let line = lines[i].trim();

        if line == "*** End Patch" {
            break;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = path.to_string();
            i += 1;
            let mut content = String::new();
            while i < lines.len() {
                let l = lines[i];
                if l.starts_with("*** ") {
                    break;
                }
                if let Some(text_line) = l.strip_prefix('+') {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(text_line);
                } else {
                    return Err(format!("Expected '+' prefix in Add File block, got: {l}"));
                }
                i += 1;
            }
            ops.push(PatchOperation::Add { path, content });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(PatchOperation::Delete {
                path: path.to_string(),
            });
            i += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = path.to_string();
            i += 1;
            let mut hunks = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                if l.starts_with("*** ") && !l.starts_with("@@ ") {
                    break;
                }
                if l.starts_with("@@ ") && l.ends_with(" @@") {
                    let context_line = l[3..l.len() - 3].to_string();
                    i += 1;
                    let mut changes = Vec::new();
                    while i < lines.len() {
                        let cl = lines[i];
                        if cl.starts_with("*** ") || (cl.starts_with("@@ ") && cl.ends_with(" @@"))
                        {
                            break;
                        }
                        if let Some(removed) = cl.strip_prefix('-') {
                            changes.push(Change::Remove(removed.to_string()));
                        } else if let Some(added) = cl.strip_prefix('+') {
                            changes.push(Change::Add(added.to_string()));
                        } else if let Some(ctx) = cl.strip_prefix(' ') {
                            changes.push(Change::Context(ctx.to_string()));
                        } else if cl.is_empty() {
                            changes.push(Change::Context(String::new()));
                        } else {
                            return Err(format!(
                                "Unexpected line in hunk (expected +, -, or space prefix): {cl}"
                            ));
                        }
                        i += 1;
                    }
                    hunks.push(Hunk {
                        context_line,
                        changes,
                    });
                } else {
                    return Err(format!("Expected @@ context @@ line, got: {l}"));
                }
            }
            ops.push(PatchOperation::Update { path, hunks });
        } else {
            return Err(format!("Unexpected line in patch: {line}"));
        }
    }

    Ok(ops)
}

/// Applies a list of patch operations using the given execution environment.
///
/// # Errors
/// Returns an error if any file operation fails.
pub async fn apply_patch_operations(
    ops: &[PatchOperation],
    env: &dyn ExecutionEnvironment,
) -> Result<String, String> {
    let mut results = Vec::new();

    for op in ops {
        match op {
            PatchOperation::Add { path, content } => {
                env.write_file(path, content).await?;
                results.push(format!("Added file: {path}"));
            }
            PatchOperation::Delete { path } => {
                env.delete_file(path).await?;
                results.push(format!("Deleted file: {path}"));
            }
            PatchOperation::Update { path, hunks } => {
                let original = env.read_file(path, None, None).await?;
                let updated = apply_hunks(&original, hunks)?;
                env.write_file(path, &updated).await?;
                results.push(format!("Updated file: {path}"));
            }
        }
    }

    Ok(results.join("\n"))
}

fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let mut lines: Vec<String> = content.lines().map(String::from).collect();

    // Apply hunks in reverse order to preserve line indices
    for hunk in hunks.iter().rev() {
        let context_pos = lines
            .iter()
            .position(|l| l.trim() == hunk.context_line.trim())
            .ok_or_else(|| {
                format!(
                    "Could not find context line in file: '{}'",
                    hunk.context_line
                )
            })?;

        // Build what we expect to find and what to replace with
        let mut new_lines: Vec<String> = Vec::new();

        // The context line itself is part of the hunk context
        // We start replacing at context_pos
        new_lines.push(lines[context_pos].clone());
        let mut file_idx = context_pos + 1;

        for change in &hunk.changes {
            match change {
                Change::Remove(_) => {
                    file_idx += 1;
                }
                Change::Add(text) => {
                    new_lines.push(text.clone());
                }
                Change::Context(_) => {
                    if file_idx < lines.len() {
                        new_lines.push(lines[file_idx].clone());
                    }
                    file_idx += 1;
                }
            }
        }

        // Calculate total lines consumed from original (context_line + removes + context changes)
        let total_original_lines = 1 + hunk
            .changes
            .iter()
            .filter(|c| matches!(c, Change::Remove(_) | Change::Context(_)))
            .count();

        // Replace the range
        let end = (context_pos + total_original_lines).min(lines.len());
        lines.splice(context_pos..end, new_lines);
    }

    Ok(lines.join("\n"))
}

fn make_apply_patch_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "apply_patch".into(),
            description: "Apply a v4a format patch to modify files".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "The patch content in v4a format"
                    }
                },
                "required": ["patch"]
            }),
        },
        executor: Arc::new(|args, env, _cancel| {
            Box::pin(async move {
                let patch_text = args
                    .get("patch")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing required parameter: patch".to_string())?;

                let ops = parse_v4a_patch(patch_text)?;
                apply_patch_operations(&ops, env.as_ref()).await
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockExecutionEnvironment, MutableMockExecutionEnvironment};
    use std::collections::HashMap;

    #[test]
    fn openai_profile_identity() {
        let profile = OpenAiProfile::new("o3-mini");
        assert_eq!(profile.id(), "openai");
        assert_eq!(profile.model(), "o3-mini");
    }

    #[test]
    fn openai_profile_capabilities() {
        let profile = OpenAiProfile::new("o3-mini");
        assert!(profile.supports_reasoning());
        assert!(profile.supports_streaming());
        assert!(profile.supports_parallel_tool_calls());
        assert_eq!(profile.context_window_size(), 128_000);
    }

    #[test]
    fn openai_system_prompt_contains_env_context() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockExecutionEnvironment::linux();
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
        let env = MockExecutionEnvironment::linux();
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
        let env = MockExecutionEnvironment::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
        assert!(prompt.contains("clean, maintainable code"));
        assert!(prompt.contains("existing code conventions"));
    }

    #[test]
    fn openai_system_prompt_includes_project_docs() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockExecutionEnvironment::linux();
        let docs = vec!["# Project README".into(), "# CONTRIBUTING guide".into()];
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &docs, None, &[]);
        assert!(prompt.contains("# Project README"));
        assert!(prompt.contains("# CONTRIBUTING guide"));
    }

    #[test]
    fn openai_system_prompt_includes_user_instructions() {
        let profile = OpenAiProfile::new("o3-mini");
        let env = MockExecutionEnvironment::linux();
        let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], Some("Always write tests first"), &[]);
        assert!(prompt.contains("Always write tests first"));
        assert!(prompt.contains("# User Instructions"));
    }

    #[test]
    fn openai_provider_options_default_none() {
        let profile = OpenAiProfile::new("o3-mini");
        assert!(profile.provider_options().is_none());
    }

    #[test]
    fn openai_provider_options_with_reasoning_effort() {
        let mut profile = OpenAiProfile::new("o3-mini");
        profile.set_reasoning_effort(Some("high".to_string()));
        let options = profile.provider_options().unwrap();
        assert_eq!(
            options,
            serde_json::json!({
                "openai": {
                    "reasoning": {
                        "effort": "high"
                    }
                }
            })
        );
    }

    #[test]
    fn openai_provider_options_cleared() {
        let mut profile = OpenAiProfile::new("o3-mini");
        profile.set_reasoning_effort(Some("high".to_string()));
        assert!(profile.provider_options().is_some());
        profile.set_reasoning_effort(None);
        assert!(profile.provider_options().is_none());
    }

    #[test]
    fn openai_subagent_tools_registered() {
        use crate::subagent::SubAgentManager;
        use crate::subagent::SessionFactory;

        let mut profile = OpenAiProfile::new("o3-mini");
        assert_eq!(profile.tool_registry().names().len(), 8);

        let manager = Arc::new(tokio::sync::Mutex::new(SubAgentManager::new(3)));
        let factory: SessionFactory = Arc::new(|| {
            panic!("should not be called in test")
        });
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
    fn parse_v4a_add_file() {
        let patch = "\
*** Begin Patch
*** Add File: src/new_file.rs
+fn main() {
+    println!(\"hello\");
+}
*** End Patch";

        let ops = parse_v4a_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0],
            PatchOperation::Add {
                path: "src/new_file.rs".into(),
                content: "fn main() {\n    println!(\"hello\");\n}".into(),
            }
        );
    }

    #[test]
    fn parse_v4a_delete_file() {
        let patch = "\
*** Begin Patch
*** Delete File: src/old_file.rs
*** End Patch";

        let ops = parse_v4a_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0],
            PatchOperation::Delete {
                path: "src/old_file.rs".into(),
            }
        );
    }

    #[test]
    fn parse_v4a_update_file() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@ fn hello() @@
-    println!(\"old\");
+    println!(\"new\");
*** End Patch";

        let ops = parse_v4a_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOperation::Update { path, hunks } => {
                assert_eq!(path, "src/lib.rs");
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].context_line, "fn hello()");
                assert_eq!(hunks[0].changes.len(), 2);
                assert_eq!(
                    hunks[0].changes[0],
                    Change::Remove("    println!(\"old\");".into())
                );
                assert_eq!(
                    hunks[0].changes[1],
                    Change::Add("    println!(\"new\");".into())
                );
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[test]
    fn parse_v4a_multi_operation() {
        let patch = "\
*** Begin Patch
*** Add File: src/a.rs
+// file a
*** Delete File: src/b.rs
*** Update File: src/c.rs
@@ fn main() @@
-    old_call();
+    new_call();
*** End Patch";

        let ops = parse_v4a_patch(patch).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], PatchOperation::Add { .. }));
        assert!(matches!(&ops[1], PatchOperation::Delete { .. }));
        assert!(matches!(&ops[2], PatchOperation::Update { .. }));
    }

    #[tokio::test]
    async fn apply_patch_add_file() {
        let env = MutableMockExecutionEnvironment::new(HashMap::new());
        let ops = vec![PatchOperation::Add {
            path: "src/new.rs".into(),
            content: "fn new() {}".into(),
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("Added file: src/new.rs"));

        let content = env.read_file("src/new.rs", None, None).await.unwrap();
        assert_eq!(content, "fn new() {}");
    }

    #[tokio::test]
    async fn apply_patch_update_file() {
        let mut files = HashMap::new();
        files.insert(
            "src/lib.rs".to_string(),
            "fn hello() {\n    println!(\"old\");\n}".to_string(),
        );
        let env = MutableMockExecutionEnvironment::new(files);

        let ops = vec![PatchOperation::Update {
            path: "src/lib.rs".into(),
            hunks: vec![Hunk {
                context_line: "fn hello() {".into(),
                changes: vec![
                    Change::Remove("    println!(\"old\");".into()),
                    Change::Add("    println!(\"new\");".into()),
                ],
            }],
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("Updated file: src/lib.rs"));

        let content = env.read_file("src/lib.rs", None, None).await.unwrap();
        assert!(content.contains("println!(\"new\")"));
        assert!(!content.contains("println!(\"old\")"));
    }
}
