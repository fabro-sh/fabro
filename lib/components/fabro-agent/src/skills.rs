use std::sync::Arc;

use fabro_llm::types::ToolDefinition;
use tokio_util::sync::CancellationToken;

use crate::error::{Error, InterruptReason};
use crate::native_tool::{NativeTool, ToolVocabulary};
use crate::sandbox::Sandbox;
use crate::tool_registry::{RegisteredTool, ToolSource};
use crate::tools::required_str;
use crate::types::{AgentEvent, SkillActivationSource};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name:        String,
    pub description: String,
    pub template:    String,
}

pub fn parse_skill(content: &str) -> Result<Skill, String> {
    let trimmed = content.trim();
    if !trimmed.starts_with("---") {
        return Err("Missing YAML frontmatter delimiters".into());
    }

    let after_first = &trimmed[3..];
    let end_idx = after_first
        .find("\n---")
        .ok_or("Missing closing frontmatter delimiter")?;
    let frontmatter = &after_first[..end_idx];
    let body = &after_first[end_idx + 4..];

    let mut name: Option<String> = None;
    let mut description = String::new();

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("description:") {
            description = val.trim().to_string();
        }
    }

    let name = name.ok_or("Missing required 'name' field in frontmatter")?;
    let template = body.trim().to_string();

    Ok(Skill {
        name,
        description,
        template,
    })
}

/// A detected skill reference in user input: the name and byte range of the
/// `/name` token.
struct SkillMatch {
    name:  String,
    /// Byte offset of the `/` character
    start: usize,
    /// Byte offset just past the skill name
    end:   usize,
}

fn is_skill_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
}

/// Find a `/skill-name` invocation at the START of the input only.
///
/// A slash command expresses the sender's intent to invoke a skill, so it is
/// only honored where that intent lives: at the very beginning of the input
/// (optionally after leading whitespace). Mid-text tokens like
/// "smoke-tested via a /tmp build" or a failure echo mentioning
/// "unknown skill: /tmp" are prose about paths or errors, not commands —
/// they routinely appear in preamble context stitched ahead of a stage
/// prompt, and expanding them either fails the stage ("Unknown skill") or
/// silently replaces the whole prompt with a matching skill's template.
fn find_skill_references(input: &str) -> Vec<SkillMatch> {
    let bytes = input.as_bytes();
    let len = bytes.len();

    // Skip leading whitespace
    let mut i = 0;
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= len || bytes[i] != b'/' {
        return Vec::new();
    }

    // The first char after `/` must be a lowercase letter
    let name_start = i + 1;
    if name_start >= len || !bytes[name_start].is_ascii_lowercase() {
        return Vec::new();
    }

    // Consume the rest of the name
    let mut j = name_start + 1;
    while j < len && is_skill_name_char(bytes[j] as char) {
        j += 1;
    }

    // The name must be followed by whitespace or end of string
    let followed_by_boundary = j >= len || bytes[j].is_ascii_whitespace();
    if !followed_by_boundary {
        return Vec::new();
    }

    vec![SkillMatch {
        name:  input[name_start..j].to_string(),
        start: i,
        end:   j,
    }]
}

#[derive(Debug)]
pub struct ExpandedInput {
    pub text:       String,
    pub skill_name: Option<String>,
}

pub fn expand_skill(skills: &[Skill], input: &str) -> Result<ExpandedInput, String> {
    let refs = find_skill_references(input);

    if refs.is_empty() {
        return Ok(ExpandedInput {
            text:       input.to_string(),
            skill_name: None,
        });
    }

    if refs.len() > 1 {
        return Err("Only one skill reference per input is allowed".into());
    }

    let skill_ref = &refs[0];

    let skill = skills
        .iter()
        .find(|s| s.name == skill_ref.name)
        .ok_or_else(|| format!("Unknown skill: /{}", skill_ref.name))?;

    // Remove the /skill-name token from input to get user_input
    let before = &input[..skill_ref.start];
    let after = &input[skill_ref.end..];
    let user_input = format!("{before}{after}").trim().to_string();

    let text = if skill.template.contains("{{user_input}}") {
        skill.template.replace("{{user_input}}", &user_input)
    } else {
        skill.template.clone()
    };

    Ok(ExpandedInput {
        text,
        skill_name: Some(skill_ref.name.clone()),
    })
}

pub fn make_use_skill_tool(skills: Arc<Vec<Skill>>) -> RegisteredTool {
    make_use_skill_tool_for_vocabulary(skills, ToolVocabulary::Fabro)
}

/// Build the skill loader with the argument schema used by `vocabulary`.
///
/// Kimi Code calls the fields `skill` and `args`; fabro's native surface uses
/// `skill_name`. The executor keeps one implementation for both.
pub fn make_use_skill_tool_for_vocabulary(
    skills: Arc<Vec<Skill>>,
    vocabulary: ToolVocabulary,
) -> RegisteredTool {
    let (name_parameter, parameters) = match vocabulary {
        // Codex has no skill-loading tool of its own -- it reads `SKILL.md`
        // through the shell -- so there is no contract to match and the Codex
        // vocabulary keeps fabro's.
        ToolVocabulary::Fabro | ToolVocabulary::Codex => (
            "skill_name",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Name of the skill to load (without the / prefix)"
                    }
                },
                "required": ["skill_name"]
            }),
        ),
        ToolVocabulary::Claude5 => (
            "skill",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "Exact name of the skill to invoke"
                    },
                    "args": {
                        "type": "string",
                        "description": "Optional argument string to pass to the skill"
                    }
                },
                "required": ["skill"],
                "additionalProperties": false
            }),
        ),
        ToolVocabulary::KimiCode => (
            "skill",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "Exact name of the skill to invoke"
                    },
                    "args": {
                        "type": "string",
                        "description": "Optional argument string to pass to the skill"
                    }
                },
                "required": ["skill"]
            }),
        ),
    };
    RegisteredTool {
        definition: ToolDefinition {
            name: NativeTool::UseSkill.canonical_name().into(),
            description: "Load a skill's instructions by name. Call this when the user's \
                          request matches an available skill."
                .into(),
            parameters,
        },
        executor:   Arc::new(move |args, ctx| {
            let skills = skills.clone();
            Box::pin(async move {
                let name = required_str(&args, name_parameter)?;
                let skill = skills
                    .iter()
                    .find(|s| s.name == name)
                    .ok_or_else(|| format!("Unknown skill: {name}"))?;
                ctx.emit_agent_event(AgentEvent::SkillActivated {
                    skill_name: name.to_string(),
                    source:     SkillActivationSource::Tool,
                });
                let skill_args = args.get("args").and_then(serde_json::Value::as_str);
                let content = match skill_args.filter(|value| !value.is_empty()) {
                    Some(value) if skill.template.contains("{{user_input}}") => {
                        skill.template.replace("{{user_input}}", value)
                    }
                    Some(value) => format!("{}\n\nARGUMENTS:\n{value}", skill.template),
                    None => skill.template.clone(),
                };
                Ok(content)
            })
        }),
        source:     ToolSource::Skill,
    }
}

/// Render the skills section of a system prompt.
pub fn format_skills_prompt_section(skills: &[Skill], vocabulary: ToolVocabulary) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let skill_tool = NativeTool::UseSkill.name(vocabulary);
    let mut lines = vec![
        "# Available Skills".to_string(),
        format!(
            "When the user's request matches a skill below, call the `{skill_tool}` tool \
             to load its instructions, then follow them."
        ),
    ];
    for skill in skills {
        if skill.description.is_empty() {
            lines.push(format!("- `{}`", skill.name));
        } else {
            lines.push(format!("- `{}`: {}", skill.name, skill.description));
        }
    }
    lines.join("\n")
}

pub fn default_skill_dirs(fabro_skills_dir: Option<&str>, git_root: Option<&str>) -> Vec<String> {
    let mut dirs = Vec::new();

    if let Some(skills_dir) = fabro_skills_dir {
        dirs.push(skills_dir.to_string());
    }

    if let Some(root) = git_root {
        dirs.push(format!("{root}/.fabro/skills"));
        dirs.push(format!("{root}/skills"));
    }

    dirs
}

pub async fn discover_skills(
    env: &dyn Sandbox,
    dirs: &[String],
    cancel_token: &CancellationToken,
) -> Result<Vec<Skill>, Error> {
    let mut skills_by_name: std::collections::HashMap<String, Skill> =
        std::collections::HashMap::new();

    for dir in dirs {
        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }
        let glob_result = env.glob("*/SKILL.md", Some(dir)).await;
        if cancel_token.is_cancelled() {
            return Err(Error::Interrupted(InterruptReason::Cancelled));
        }
        let Ok(paths) = glob_result else {
            continue;
        };

        for path in paths {
            if cancel_token.is_cancelled() {
                return Err(Error::Interrupted(InterruptReason::Cancelled));
            }
            let read_result = env.read_file_text(&path).await;
            if cancel_token.is_cancelled() {
                return Err(Error::Interrupted(InterruptReason::Cancelled));
            }
            let Ok(content) = read_result else {
                continue;
            };

            if let Ok(skill) = parse_skill(&content) {
                skills_by_name.insert(skill.name.clone(), skill);
            }
        }
    }

    let mut skills: Vec<Skill> = skills_by_name.into_values().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;
    use crate::tool_registry::ToolContext;

    // --- parse_skill tests ---

    #[test]
    fn parse_skill_basic() {
        let content = "\
---
name: commit
description: Create a git commit following best practices
---

Review staged and unstaged changes, then create a well-crafted commit.

{{user_input}}";

        let skill = parse_skill(content).unwrap();
        assert_eq!(skill.name, "commit");
        assert_eq!(
            skill.description,
            "Create a git commit following best practices"
        );
        assert!(skill.template.contains("Review staged"));
        assert!(skill.template.contains("{{user_input}}"));
    }

    #[test]
    fn parse_skill_no_frontmatter() {
        let content = "Just some markdown without frontmatter";
        let result = parse_skill(content);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("frontmatter"));
    }

    #[test]
    fn parse_skill_missing_name() {
        let content = "\
---
description: A skill without a name
---

Some template";

        let result = parse_skill(content);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("name"));
    }

    #[test]
    fn parse_skill_description_optional() {
        let content = "\
---
name: simple
---

Just a template";

        let skill = parse_skill(content).unwrap();
        assert_eq!(skill.name, "simple");
        assert_eq!(skill.description, "");
        assert_eq!(skill.template, "Just a template");
    }

    #[test]
    fn parse_skill_trims_template() {
        let content = "\
---
name: trimmed
---


  Body with leading/trailing whitespace


";

        let skill = parse_skill(content).unwrap();
        assert_eq!(skill.template, "Body with leading/trailing whitespace");
    }

    // --- expand_skill tests ---

    fn test_skills() -> Vec<Skill> {
        vec![
            Skill {
                name:        "commit".into(),
                description: "Create a commit".into(),
                template:    "Review changes and commit.\n\n{{user_input}}".into(),
            },
            Skill {
                name:        "test".into(),
                description: "Run tests".into(),
                template:    "Run the test suite.".into(),
            },
        ]
    }

    #[test]
    fn expand_no_skill_reference() {
        let skills = test_skills();
        let result = expand_skill(&skills, "just some plain text").unwrap();
        assert_eq!(result.text, "just some plain text");
        assert_eq!(result.skill_name, None);
    }

    #[test]
    fn expand_skill_at_start() {
        let skills = test_skills();
        let result = expand_skill(&skills, "/commit do the thing").unwrap();
        assert_eq!(result.text, "Review changes and commit.\n\ndo the thing");
        assert_eq!(result.skill_name.as_deref(), Some("commit"));
    }

    // Mid-line references are intentionally NOT expanded anymore: prose
    // containing slash-tokens is context, not a command. See
    // find_skill_references — only input-leading invocations are honored.
    #[test]
    fn expand_skill_mid_line_is_not_a_command() {
        let skills = test_skills();
        let input = "please /commit the auth changes";
        let result = expand_skill(&skills, input).unwrap();
        assert_eq!(result.text, input);
        assert_eq!(result.skill_name, None);
    }

    #[test]
    fn expand_skill_alone() {
        let skills = test_skills();
        let result = expand_skill(&skills, "/commit").unwrap();
        assert_eq!(result.text, "Review changes and commit.\n\n");
        assert_eq!(result.skill_name.as_deref(), Some("commit"));
    }

    #[test]
    fn expand_unknown_skill() {
        let skills = test_skills();
        let result = expand_skill(&skills, "/nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown skill"));
    }

    #[test]
    fn expand_does_not_match_paths() {
        let skills = test_skills();
        let result = expand_skill(&skills, "/usr/bin/bash").unwrap();
        assert_eq!(result.text, "/usr/bin/bash");
        assert_eq!(result.skill_name, None);
    }

    // Real-world regressions: agents mentioning lowercase absolute paths
    // in their output. The path text re-entered a later stage's input
    // (preamble context) and was parsed as a skill reference, failing the
    // stage with "Unknown skill: /tmp" (run 01M0N6ZAP0NAWMWYWKZZ7T7R2B,
    // planner pass 2; run 01M0NGQXB67674XQ5YCR1MB4BN, implementer pass 1).
    #[test]
    fn expand_tolerates_bare_temp_dir_token_from_context() {
        let skills = test_skills();
        let input = "Prior stage summary: smoke-tested via a /tmp build. \
The gate was green.";
        let result = expand_skill(&skills, input);
        assert!(
            result.is_ok(),
            "bare lowercase path tokens from prior-stage context must not fail expansion: {result:?}"
        );
        let expanded = result.unwrap();
        assert_eq!(expanded.text, input);
        assert_eq!(expanded.skill_name, None);
    }

    // A known skill name inside prior-stage context must not silently
    // replace the whole stage prompt with the skill template.
    #[test]
    fn expand_does_not_hijack_on_known_skill_named_in_context() {
        let skills = test_skills();
        let input = "Prior review feedback: run the /commit checks yourself \
before finishing this pass. Now implement feature X.";
        let result = expand_skill(&skills, input).unwrap();
        assert_eq!(
            result.text, input,
            "a skill name inside prior-stage context must not replace the stage prompt"
        );
        assert_eq!(result.skill_name, None);
    }

    // The engine's own failure echo re-enters the next stage's preamble
    // (failure_signature carries the literal error text) and crashed every
    // subsequent stage of the run.
    #[test]
    fn expand_tolerates_engine_failure_echo_of_unknown_skill() {
        let skills = test_skills();
        let input = "failure_signature: implementer|structural|precondition \
failed: agent session failed: invalid state: unknown skill: /tmp";
        let result = expand_skill(&skills, input);
        assert!(
            result.is_ok(),
            "engine failure echo must not fail expansion: {result:?}"
        );
        assert_eq!(result.unwrap().skill_name, None);
    }

    // With start-only expansion, a leading command plus a mid-line token is
    // one command with prose that mentions another skill name: the leading
    // one wins, the rest is user_input. No error.
    #[test]
    fn expand_leading_command_wins_over_mid_line_mention() {
        let skills = test_skills();
        let result = expand_skill(&skills, "/commit and /test").unwrap();
        assert_eq!(result.skill_name.as_deref(), Some("commit"));
        assert!(result.text.starts_with("Review changes and commit."));
        assert!(result.text.contains("and /test"));
    }

    #[test]
    fn expand_template_without_placeholder() {
        let skills = test_skills();
        let result = expand_skill(&skills, "/test please run").unwrap();
        assert_eq!(result.text, "Run the test suite.");
        assert_eq!(result.skill_name.as_deref(), Some("test"));
    }

    // --- format_skills_prompt_section tests ---

    #[test]
    fn format_empty() {
        assert_eq!(format_skills_prompt_section(&[], ToolVocabulary::Fabro), "");
    }

    #[test]
    fn format_lists_skills() {
        let skills = test_skills();
        let section = format_skills_prompt_section(&skills, ToolVocabulary::Fabro);
        assert!(section.contains("# Available Skills"));
        assert!(section.contains("call the `use_skill` tool"));
        assert!(section.contains("- `commit`: Create a commit"));
        assert!(section.contains("- `test`: Run tests"));
    }

    // --- discover_skills tests ---

    #[tokio::test]
    async fn discover_loads_files() {
        let mut files = HashMap::new();
        files.insert(
            "/skills/commit/SKILL.md".into(),
            "---\nname: commit\ndescription: Make a commit\n---\nDo commit".into(),
        );
        let env = MockSandbox {
            files,
            glob_results: vec!["/skills/commit/SKILL.md".into()],
            ..Default::default()
        };

        let skills = discover_skills(&env, &["/skills".into()], &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "commit");
        assert_eq!(skills[0].description, "Make a commit");
    }

    #[tokio::test]
    async fn discover_skips_invalid() {
        let mut files = HashMap::new();
        files.insert(
            "/skills/good/SKILL.md".into(),
            "---\nname: good\n---\nGood template".into(),
        );
        files.insert("/skills/bad/SKILL.md".into(), "no frontmatter here".into());
        let env = MockSandbox {
            files,
            glob_results: vec![
                "/skills/good/SKILL.md".into(),
                "/skills/bad/SKILL.md".into(),
            ],
            ..Default::default()
        };

        let skills = discover_skills(&env, &["/skills".into()], &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "good");
    }

    #[tokio::test]
    async fn discover_empty_dirs() {
        let env = MockSandbox::default();
        let skills = discover_skills(&env, &[], &CancellationToken::new())
            .await
            .unwrap();
        assert!(skills.is_empty());
    }

    #[tokio::test]
    async fn discover_project_overrides_global() {
        let mut files = HashMap::new();
        files.insert(
            "/global/commit/SKILL.md".into(),
            "---\nname: commit\ndescription: Global commit\n---\nGlobal template".into(),
        );
        files.insert(
            "/project/commit/SKILL.md".into(),
            "---\nname: commit\ndescription: Project commit\n---\nProject template".into(),
        );

        // We need separate envs because MockSandbox returns the same glob_results
        // for all calls. Instead, we test with a single env that has both files
        // and glob returns both — the later dir overrides the earlier.
        let env = MockSandbox {
            files,
            glob_results: vec![
                "/global/commit/SKILL.md".into(),
                "/project/commit/SKILL.md".into(),
            ],
            ..Default::default()
        };

        // discover_skills iterates dirs in order; later dirs override earlier names
        let skills = discover_skills(
            &env,
            &["/global".into(), "/project".into()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Project commit");
    }

    // --- default_skill_dirs tests ---

    #[test]
    fn default_dirs_with_git_root() {
        let dirs = default_skill_dirs(Some("/home/user/.fabro/skills"), Some("/repo"));
        assert_eq!(dirs, vec![
            "/home/user/.fabro/skills",
            "/repo/.fabro/skills",
            "/repo/skills",
        ]);
    }

    #[test]
    fn default_dirs_without_git_root() {
        let dirs = default_skill_dirs(Some("/home/user/.fabro/skills"), None);
        assert_eq!(dirs, vec!["/home/user/.fabro/skills"]);
    }

    // --- make_use_skill_tool tests ---

    #[tokio::test]
    async fn use_skill_tool_returns_template() {
        let skills = Arc::new(test_skills());
        let tool = make_use_skill_tool(skills);

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let args = serde_json::json!({"skill_name": "commit"});
        let ctx = ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        };
        let result = (tool.executor)(args, ctx).await;
        assert_eq!(
            result.unwrap(),
            "Review changes and commit.\n\n{{user_input}}"
        );
    }

    #[tokio::test]
    async fn use_skill_tool_unknown_skill_errors() {
        let skills = Arc::new(test_skills());
        let tool = make_use_skill_tool(skills);

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let args = serde_json::json!({"skill_name": "nonexistent"});
        let ctx = ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        };
        let result = (tool.executor)(args, ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown skill"));
    }

    #[tokio::test]
    async fn use_skill_tool_missing_param_errors() {
        let skills = Arc::new(test_skills());
        let tool = make_use_skill_tool(skills);

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let args = serde_json::json!({});
        let ctx = ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        };
        let result = (tool.executor)(args, ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing required parameter"));
    }

    #[tokio::test]
    async fn kimi_skill_schema_and_args_match_kimi_code() {
        let skills = Arc::new(test_skills());
        let tool = make_use_skill_tool_for_vocabulary(skills, ToolVocabulary::KimiCode);
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

        let result = (tool.executor)(
            serde_json::json!({"skill": "commit", "args": "only staged files"}),
            ctx,
        )
        .await
        .unwrap();

        assert!(result.contains("only staged files"), "{result}");
        assert!(
            tool.definition.parameters["properties"]
                .get("skill")
                .is_some()
        );
        assert!(
            tool.definition.parameters["properties"]
                .get("args")
                .is_some()
        );
        assert!(
            tool.definition.parameters["properties"]
                .get("skill_name")
                .is_none()
        );
    }

    #[tokio::test]
    async fn claude5_skill_schema_uses_skill_and_optional_args() {
        let skills = Arc::new(test_skills());
        let tool = make_use_skill_tool_for_vocabulary(skills, ToolVocabulary::Claude5);
        let result = (tool.executor)(
            serde_json::json!({"skill": "commit", "args": "only staged files"}),
            ToolContext {
                env:                 Arc::new(MockSandbox::default()),
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await
        .unwrap();

        assert!(result.contains("only staged files"), "{result}");
        assert_eq!(
            tool.definition.parameters["required"],
            serde_json::json!(["skill"])
        );
        assert_eq!(tool.definition.parameters["additionalProperties"], false);
        assert!(
            tool.definition.parameters["properties"]
                .get("skill")
                .is_some()
        );
        assert!(
            tool.definition.parameters["properties"]
                .get("args")
                .is_some()
        );
        assert!(
            tool.definition.parameters["properties"]
                .get("skill_name")
                .is_none()
        );
    }
}
