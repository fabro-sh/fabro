use std::path::Path;

use anyhow::{bail, Result};
use tracing::{debug, info};

use crate::args::{SkillDir, SkillInstallArgs, SkillScope};

const SKILL_MD: &str = include_str!("../../../../../../skills/fabro-create-workflow/SKILL.md");
const REF_DOT_LANGUAGE: &str =
    include_str!("../../../../../../skills/fabro-create-workflow/references/dot-language.md");
const REF_EXAMPLE_WORKFLOWS: &str =
    include_str!("../../../../../../skills/fabro-create-workflow/references/example-workflows.md");
const REF_RUN_CONFIGURATION: &str =
    include_str!("../../../../../../skills/fabro-create-workflow/references/run-configuration.md");

const SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", SKILL_MD),
    ("references/dot-language.md", REF_DOT_LANGUAGE),
    ("references/example-workflows.md", REF_EXAMPLE_WORKFLOWS),
    ("references/run-configuration.md", REF_RUN_CONFIGURATION),
];

/// Install all skill files under `base_dir/fabro-create-workflow/`.
pub fn install_skill_to(base_dir: &Path) -> Result<()> {
    let skill_dir = base_dir.join("fabro-create-workflow");

    for (rel_path, content) in SKILL_FILES {
        let dest = skill_dir.join(rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        debug!(file = %rel_path, "Writing skill file");
        std::fs::write(&dest, content)?;
    }

    info!(path = %skill_dir.display(), "Skill installed");
    Ok(())
}

pub fn run_skill_install(args: &SkillInstallArgs) -> Result<()> {
    let base_dir = resolve_base_dir(&args.scope, &args.dir)?;
    let skill_dir = base_dir.join("fabro-create-workflow");

    if skill_dir.exists() && !args.force {
        let confirm = dialoguer::Confirm::new()
            .with_prompt(format!(
                "Skill directory already exists at {}. Overwrite?",
                skill_dir.display()
            ))
            .default(false)
            .interact()?;

        if !confirm {
            bail!("Aborted: skill directory already exists");
        }
    }

    install_skill_to(&base_dir)
}

fn resolve_base_dir(scope: &SkillScope, dir: &SkillDir) -> Result<std::path::PathBuf> {
    let dir_name = match dir {
        SkillDir::Claude => ".claude",
        SkillDir::Agents => ".agents",
    };

    let root = match scope {
        SkillScope::User => {
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?
        }
        SkillScope::Project => std::env::current_dir()?,
    };

    Ok(root.join(dir_name).join("skills"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_files_are_non_empty() {
        assert!(!SKILL_MD.is_empty());
        assert!(!REF_DOT_LANGUAGE.is_empty());
        assert!(!REF_EXAMPLE_WORKFLOWS.is_empty());
        assert!(!REF_RUN_CONFIGURATION.is_empty());
    }

    #[test]
    fn install_writes_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("skills");

        install_skill_to(&base).unwrap();

        for (rel_path, content) in SKILL_FILES {
            let path = base.join("fabro-create-workflow").join(rel_path);
            assert!(path.exists(), "Missing file: {rel_path}");
            let written = std::fs::read_to_string(&path).unwrap();
            assert_eq!(written, *content, "Content mismatch: {rel_path}");
        }
    }

    #[test]
    fn install_overwrites_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("skills");

        install_skill_to(&base).unwrap();

        let sentinel_path = base.join("fabro-create-workflow/SKILL.md");
        std::fs::write(&sentinel_path, "old content").unwrap();

        install_skill_to(&base).unwrap();

        let content = std::fs::read_to_string(&sentinel_path).unwrap();
        assert_eq!(content, SKILL_MD);
    }
}
