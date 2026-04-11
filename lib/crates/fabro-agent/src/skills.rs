use std::sync::Arc;

use fabro_llm::types::ToolDefinition;

use crate::sandbox::Sandbox;
use crate::tool_registry::RegisteredTool;
use crate::tools::required_str;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub template: String,
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
    name: String,
    /// Byte offset of the `/` character
    start: usize,
    /// Byte offset just past the skill name
    end: usize,
}

fn is_skill_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
}

/// Find all `/skill-name` tokens in input where the `/` is preceded by
/// whitespace (or start-of-string) and the name is followed by whitespace (or
/// end-of-string).
fn find_skill_references(input: &str) -> Vec<SkillMatch> {
    let mut results = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'/' {
            // Check that preceding char is whitespace or this is start of string
            let preceded_by_boundary = i == 0 || bytes[i - 1].is_ascii_whitespace();
            if !preceded_by_boundary {
                i += 1;
                continue;
            }

            // The first char after `/` must be a lowercase letter
            let name_start = i + 1;
            if name_start >= len || !bytes[name_start].is_ascii_lowercase() {
                i += 1;
                continue;
            }

            // Consume the rest of the name
            let mut j = name_start + 1;
            while j < len && is_skill_name_char(bytes[j] as char) {
                j += 1;
            }

            // Check that following char is whitespace or end of string
            let followed_by_boundary = j >= len || bytes[j].is_ascii_whitespace();
            if followed_by_boundary {
                results.push(SkillMatch {
                    name: input[name_start..j].to_string(),
                    start: i,
                    end: j,
                });
            }

            i = j;
        } else {
            i += 1;
        }
    }

    results
}

#[derive(Debug)]
pub struct ExpandedInput {
    pub text: String,
    pub skill_name: Option<String>,
}

pub fn expand_skill(skills: &[Skill], input: &str) -> Result<ExpandedInput, String> {
    let refs = find_skill_references(input);

    if refs.is_empty() {
        return Ok(ExpandedInput {
            text: input.to_string(),
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
    RegisteredTool {
        definition: ToolDefinition {
            name: "use_skill".into(),
            description: "Load a skill's instructions by name. Call this when the user's \
                          request matches an available skill."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Name of the skill to load (without the / prefix)"
                    }
                },
                "required": ["skill_name"]
            }),
        },
        executor: Arc::new(move |args, _ctx| {
            let skills = skills.clone();
            Box::pin(async move {
                let name = required_str(&args, "skill_name")?;
                let skill = skills
                    .iter()
                    .find(|s| s.name == name)
                    .ok_or_else(|| format!("Unknown skill: {name}"))?;
                Ok(skill.template.clone())
            })
        }),
    }
}

pub fn format_skills_prompt_section(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "# Available Skills".to_string(),
        "When the user's request matches a skill below, call the `use_skill` tool \
         to load its instructions, then follow them."
            .to_string(),
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

pub async fn discover_skills(env: &dyn Sandbox, dirs: &[String]) -> Vec<Skill> {
    let mut skills_by_name: std::collections::HashMap<String, Skill> =
        std::collections::HashMap::new();

    for dir in dirs {
        let Ok(paths) = env.glob("*/SKILL.md", Some(dir)).await else {
            continue;
        };

        for path in paths {
            let Ok(content) = env.read_file(&path, None, None).await else {
                continue;
            };

            if let Ok(skill) = parse_skill(&content) {
                skills_by_name.insert(skill.name.clone(), skill);
            }
        }
    }

    let mut skills: Vec<Skill> = skills_by_name.into_values().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
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
                name: "commit".into(),
                description: "Create a commit".into(),
                template: "Review changes and commit.\n\n{{user_input}}".into(),
            },
            Skill {
                name: "test".into(),
                description: "Run tests".into(),
                template: "Run the test suite.".into(),
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

    #[test]
    fn expand_skill_mid_line() {
        let skills = test_skills();
        let result = expand_skill(&skills, "please /commit the auth changes").unwrap();
        assert_eq!(
            result.text,
            "Review changes and commit.\n\nplease  the auth changes"
        );
        assert_eq!(result.skill_name.as_deref(), Some("commit"));
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

    #[test]
    fn expand_multiple_skills_errors() {
        let skills = test_skills();
        let result = expand_skill(&skills, "/commit and /test");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Only one skill"));
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
        assert_eq!(format_skills_prompt_section(&[]), "");
    }

    #[test]
    fn format_lists_skills() {
        let skills = test_skills();
        let section = format_skills_prompt_section(&skills);
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

        let skills = discover_skills(&env, &["/skills".into()]).await;
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

        let skills = discover_skills(&env, &["/skills".into()]).await;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "good");
    }

    #[tokio::test]
    async fn discover_empty_dirs() {
        let env = MockSandbox::default();
        let skills = discover_skills(&env, &[]).await;
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
        let skills = discover_skills(&env, &["/global".into(), "/project".into()]).await;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Project commit");
    }

    // --- default_skill_dirs tests ---

    #[test]
    fn default_dirs_with_git_root() {
        let dirs = default_skill_dirs(Some("/home/user/.fabro/skills"), Some("/repo"));
        assert_eq!(
            dirs,
            vec![
                "/home/user/.fabro/skills",
                "/repo/.fabro/skills",
                "/repo/skills",
            ]
        );
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
            tool_env: None,
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
            tool_env: None,
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
            tool_env: None,
        };
        let result = (tool.executor)(args, ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing required parameter"));
    }
}
