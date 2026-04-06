use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use fabro_config::project as project_config;
use fabro_types::Settings;
use fabro_util::path::expand_tilde;

use crate::file_resolver::{FileResolver, FilesystemFileResolver};
use crate::workflow_bundle::BundledWorkflow;

#[derive(Clone, Debug)]
pub enum WorkflowInput {
    Path(PathBuf),
    DotSource {
        source: String,
        base_dir: Option<PathBuf>,
    },
    Bundled(BundledWorkflow),
}

#[derive(Clone, Debug)]
pub(crate) struct ResolveWorkflowInput {
    pub workflow: WorkflowInput,
    pub settings: Settings,
    pub cwd: PathBuf,
}

#[derive(Clone)]
pub(crate) struct ResolvedWorkflow {
    pub raw_source: String,
    pub settings: Settings,
    pub workflow_slug: Option<String>,
    pub workflow_toml_path: Option<PathBuf>,
    pub dot_path: Option<PathBuf>,
    pub current_dir: Option<PathBuf>,
    pub file_resolver: Option<Arc<dyn FileResolver>>,
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

fn workflow_slug_from_path(workflow_path: &Path) -> Option<String> {
    let file_name = workflow_path.file_name()?.to_string_lossy();
    if workflow_path.extension().is_none() {
        return Some(file_name.into_owned());
    }

    let file_stem = workflow_path.file_stem()?.to_string_lossy();
    if file_stem == "workflow" {
        return workflow_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| Some(file_stem.into_owned()));
    }

    Some(file_stem.into_owned())
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
            let current_dir = resolution
                .dot_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();

            Ok(ResolvedWorkflow {
                raw_source,
                settings,
                workflow_slug: resolution.workflow_slug,
                workflow_toml_path: resolution.workflow_toml_path,
                dot_path: Some(resolution.dot_path.clone()),
                current_dir: Some(current_dir),
                file_resolver: Some(Arc::new(FilesystemFileResolver::new(
                    dirs::home_dir().map(|home| home.join(".fabro")),
                ))),
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
            let has_base_dir = base_dir.is_some();
            Ok(ResolvedWorkflow {
                raw_source: source,
                settings,
                workflow_slug: None,
                workflow_toml_path: None,
                dot_path: None,
                current_dir: base_dir,
                file_resolver: has_base_dir.then(|| {
                    Arc::new(FilesystemFileResolver::new(
                        dirs::home_dir().map(|home| home.join(".fabro")),
                    )) as Arc<dyn FileResolver>
                }),
                goal_override,
                working_directory,
            })
        }
        WorkflowInput::Bundled(workflow) => {
            let settings = request.settings;
            let working_directory =
                project_config::resolve_working_directory(&settings, &request.cwd);

            Ok(ResolvedWorkflow {
                raw_source: workflow.source.clone(),
                settings: settings.clone(),
                workflow_slug: workflow_slug_from_path(&workflow.logical_path),
                workflow_toml_path: None,
                dot_path: Some(workflow.logical_path.clone()),
                current_dir: Some(workflow.current_dir()),
                file_resolver: Some(workflow.file_resolver()),
                goal_override: settings.goal.clone(),
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
