#![expect(
    clippy::disallowed_methods,
    reason = "sync workflow operation loader; runs at workflow-load time"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use fabro_config::project::{
    WorkflowLocation, resolve_working_directory_from_run, workflow_slug_from_path,
};
use fabro_config::run::resolve_run_goal_from_namespace;
use fabro_types::WorkflowSettings;

use crate::file_resolver::{FileResolver, FilesystemFileResolver};
use crate::workflow_bundle::BundledWorkflow;

#[derive(Clone, Debug)]
pub enum WorkflowInput {
    Path(PathBuf),
    DotSource {
        source:   String,
        base_dir: Option<PathBuf>,
    },
    Bundled(BundledWorkflow),
}

#[derive(Clone, Debug)]
pub(crate) struct ResolveWorkflowInput {
    pub workflow: WorkflowInput,
    pub settings: WorkflowSettings,
    pub cwd:      PathBuf,
}

#[derive(Clone)]
pub(crate) struct ResolvedWorkflow {
    pub raw_source:         String,
    pub settings:           WorkflowSettings,
    pub workflow_slug:      Option<String>,
    pub workflow_toml_path: Option<PathBuf>,
    pub dot_path:           Option<PathBuf>,
    pub current_dir:        Option<PathBuf>,
    pub file_resolver:      Option<Arc<dyn FileResolver>>,
    pub goal_override:      Option<String>,
    pub working_directory:  PathBuf,
}

pub(crate) fn resolve_workflow(request: ResolveWorkflowInput) -> anyhow::Result<ResolvedWorkflow> {
    match request.workflow {
        WorkflowInput::Path(workflow_path) => {
            let location = WorkflowLocation::resolve(&workflow_path, &request.cwd)?;
            let settings = request.settings;
            let raw_source = std::fs::read_to_string(&location.graph)
                .with_context(|| format!("Failed to read {}", location.graph.display()))?;
            let working_directory = resolve_working_directory_from_run(&settings.run, &request.cwd);
            let goal_override = resolve_goal_override(&settings, &working_directory)?;

            Ok(ResolvedWorkflow {
                raw_source,
                settings,
                workflow_slug: location.slug,
                workflow_toml_path: location.toml,
                dot_path: Some(location.graph),
                current_dir: Some(location.dir),
                file_resolver: Some(Arc::new(FilesystemFileResolver::new(Some(
                    fabro_util::Home::from_env().root().to_path_buf(),
                )))),
                goal_override,
                working_directory,
            })
        }
        WorkflowInput::DotSource { source, base_dir } => {
            let settings = request.settings;
            let working_directory = resolve_working_directory_from_run(&settings.run, &request.cwd);
            let goal_override = resolve_goal_override(&settings, &working_directory)?;
            let has_base_dir = base_dir.is_some();
            Ok(ResolvedWorkflow {
                raw_source: source,
                settings,
                workflow_slug: None,
                workflow_toml_path: None,
                dot_path: None,
                current_dir: base_dir,
                file_resolver: has_base_dir.then(|| {
                    Arc::new(FilesystemFileResolver::new(Some(
                        fabro_util::Home::from_env().root().to_path_buf(),
                    ))) as Arc<dyn FileResolver>
                }),
                goal_override,
                working_directory,
            })
        }
        WorkflowInput::Bundled(workflow) => {
            let settings = request.settings;
            let working_directory = resolve_working_directory_from_run(&settings.run, &request.cwd);
            let goal_override = resolve_goal_override(&settings, &working_directory)?;

            Ok(ResolvedWorkflow {
                raw_source: workflow.source.clone(),
                settings,
                workflow_slug: workflow_slug_from_path(workflow.path.as_path()),
                workflow_toml_path: None,
                dot_path: Some(workflow.path.as_path().to_path_buf()),
                current_dir: Some(workflow.current_dir()),
                file_resolver: Some(workflow.file_resolver()),
                goal_override,
                working_directory,
            })
        }
    }
}

/// Resolve the `run.goal` override for a direct (non-manifest) workflow
/// run. Reads the file from disk if the goal layer is the `file` variant.
/// Relative paths that survived config load (e.g. env-interpolated ones)
/// are anchored at `working_directory`.
fn resolve_goal_override(
    settings: &WorkflowSettings,
    working_directory: &Path,
) -> anyhow::Result<Option<String>> {
    resolve_run_goal_from_namespace(&settings.run, working_directory)
        .map(|opt| opt.map(|resolved| resolved.text))
        .map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workflow_uses_explicit_cwd_for_relative_work_dir() {
        use fabro_types::settings::InterpString;
        use fabro_types::settings::run::RunNamespace;

        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_workflow(ResolveWorkflowInput {
            workflow: WorkflowInput::DotSource {
                source:   "digraph Test { start -> exit }".to_string(),
                base_dir: None,
            },
            settings: WorkflowSettings {
                run: RunNamespace {
                    working_dir: Some(InterpString::parse("workspace")),
                    ..RunNamespace::default()
                },
                ..WorkflowSettings::default()
            },
            cwd:      dir.path().to_path_buf(),
        })
        .unwrap();

        assert_eq!(resolved.working_directory, dir.path().join("workspace"));
    }

    #[test]
    fn resolve_workflow_reads_goal_override_from_dense_run_settings() {
        use fabro_types::settings::InterpString;
        use fabro_types::settings::run::{RunGoal, RunNamespace};

        let dir = tempfile::tempdir().unwrap();
        let goal_path = dir.path().join("goal.md");
        std::fs::write(&goal_path, "dense goal").unwrap();
        let resolved = resolve_workflow(ResolveWorkflowInput {
            workflow: WorkflowInput::DotSource {
                source:   "digraph Test { start -> exit }".to_string(),
                base_dir: None,
            },
            settings: WorkflowSettings {
                run: RunNamespace {
                    goal: Some(RunGoal::File(InterpString::parse(
                        &goal_path.display().to_string(),
                    ))),
                    ..RunNamespace::default()
                },
                ..WorkflowSettings::default()
            },
            cwd:      dir.path().to_path_buf(),
        })
        .unwrap();

        assert_eq!(resolved.goal_override.as_deref(), Some("dense goal"));
    }

    #[test]
    fn resolve_workflow_uses_dense_settings_without_re_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_workflow(ResolveWorkflowInput {
            workflow: WorkflowInput::DotSource {
                source:   "digraph Test { start -> exit }".to_string(),
                base_dir: None,
            },
            settings: WorkflowSettings::default(),
            cwd:      dir.path().to_path_buf(),
        })
        .unwrap();

        assert_eq!(resolved.settings, WorkflowSettings::default());
    }
}
