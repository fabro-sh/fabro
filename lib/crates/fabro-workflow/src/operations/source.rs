use std::path::{Path, PathBuf};

use anyhow::Context;
use fabro_config::project as project_config;
use fabro_types::Settings;
use fabro_util::path::expand_tilde;

#[derive(Clone, Debug)]
pub enum WorkflowInput {
    Path(PathBuf),
    DotSource {
        source: String,
        base_dir: Option<PathBuf>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ResolveWorkflowInput {
    pub workflow: WorkflowInput,
    pub settings: Settings,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedWorkflow {
    pub raw_source: String,
    pub settings: Settings,
    pub workflow_slug: Option<String>,
    pub workflow_toml_path: Option<PathBuf>,
    pub dot_path: Option<PathBuf>,
    pub base_dir: Option<PathBuf>,
    pub goal_override: Option<String>,
    pub working_directory: PathBuf,
}

fn resolve_goal_file(
    goal_file: Option<&Path>,
    working_directory: &Path,
) -> anyhow::Result<Option<String>> {
    let Some(goal_file) = goal_file else {
        return Ok(None);
    };
    let expanded = expand_tilde(goal_file);
    let goal_path = if expanded.is_absolute() {
        expanded
    } else {
        working_directory.join(expanded)
    };
    let content = std::fs::read_to_string(&goal_path)
        .with_context(|| format!("failed to read goal file: {}", goal_path.display()))?;
    tracing::debug!(path = %goal_path.display(), "Goal loaded from file");
    Ok(Some(content))
}

pub(crate) fn resolve_workflow(request: ResolveWorkflowInput) -> anyhow::Result<ResolvedWorkflow> {
    match request.workflow {
        WorkflowInput::Path(workflow_path) => {
            let resolution = project_config::resolve_workflow_path(&workflow_path, &request.cwd)?;
            let settings = request.settings;
            let working_directory =
                project_config::resolve_working_directory(&settings, &request.cwd);
            let raw_source = std::fs::read_to_string(&resolution.dot_path)
                .with_context(|| format!("Failed to read {}", resolution.dot_path.display()))?;
            let goal_override = settings.goal.clone().or(resolve_goal_file(
                settings.goal_file.as_deref(),
                &working_directory,
            )?);

            Ok(ResolvedWorkflow {
                raw_source,
                settings,
                workflow_slug: resolution.workflow_slug,
                workflow_toml_path: resolution.workflow_toml_path,
                dot_path: Some(resolution.dot_path.clone()),
                base_dir: Some(
                    resolution
                        .dot_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .to_path_buf(),
                ),
                goal_override,
                working_directory,
            })
        }
        WorkflowInput::DotSource { source, base_dir } => {
            let settings = request.settings;
            let working_directory =
                project_config::resolve_working_directory(&settings, &request.cwd);
            let goal_override = settings.goal.clone().or(resolve_goal_file(
                settings.goal_file.as_deref(),
                &working_directory,
            )?);
            Ok(ResolvedWorkflow {
                raw_source: source,
                settings,
                workflow_slug: None,
                workflow_toml_path: None,
                dot_path: None,
                base_dir,
                goal_override,
                working_directory,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workflow_uses_explicit_cwd_for_relative_work_dir() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_workflow(ResolveWorkflowInput {
            workflow: WorkflowInput::DotSource {
                source: "digraph Test { start -> exit }".to_string(),
                base_dir: None,
            },
            settings: Settings {
                work_dir: Some("workspace".to_string()),
                ..Default::default()
            },
            cwd: dir.path().to_path_buf(),
        })
        .unwrap();

        assert_eq!(resolved.working_directory, dir.path().join("workspace"));
    }
}
