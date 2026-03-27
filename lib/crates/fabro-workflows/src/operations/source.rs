use std::path::{Path, PathBuf};

use anyhow::Context;
use fabro_config::{project as project_config, run as run_config, FabroConfig, FabroSettings};

const RUN_GRAPH_FILE: &str = "workflow.fabro";
const LEGACY_RUN_GRAPH_FILE: &str = "graph.fabro";

#[derive(Clone, Debug)]
pub enum WorkflowInput {
    Path(PathBuf),
    DotSource {
        source: String,
        base_dir: Option<PathBuf>,
        workflow_slug: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct WorkflowPathResolution {
    pub resolved_workflow_path: PathBuf,
    pub dot_path: PathBuf,
    pub workflow_config: Option<FabroConfig>,
    pub workflow_toml_path: Option<PathBuf>,
    pub workflow_slug: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ResolveWorkflowRequest {
    pub workflow: WorkflowInput,
    pub settings: FabroSettings,
}

#[derive(Clone, Debug)]
pub struct ResolvedWorkflow {
    pub raw_source: String,
    pub settings: FabroSettings,
    pub workflow_slug: Option<String>,
    pub workflow_toml_path: Option<PathBuf>,
    pub dot_path: Option<PathBuf>,
    pub resolved_workflow_path: Option<PathBuf>,
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
    let expanded = fabro_util::path::expand_tilde(goal_file);
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

fn resolve_working_directory(settings: &FabroSettings, caller_cwd: &Path) -> PathBuf {
    let Some(work_dir) = settings.work_dir.as_deref() else {
        return caller_cwd.to_path_buf();
    };
    let path = PathBuf::from(work_dir);
    if path.is_absolute() {
        path
    } else {
        caller_cwd.join(path)
    }
}

pub fn workflow_slug_from_path(workflow_path: &Path) -> Option<String> {
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

pub fn resolve_workflow_path(workflow_path: &Path) -> anyhow::Result<WorkflowPathResolution> {
    let path = project_config::resolve_workflow_arg(workflow_path)?;
    let workflow_slug = workflow_slug_from_path(&path);
    if path.extension().is_some_and(|ext| ext == "toml") {
        match run_config::load_run_config(&path) {
            Ok(cfg) => {
                let dot_path = run_config::resolve_graph_path(
                    &path,
                    cfg.graph.as_deref().unwrap_or(RUN_GRAPH_FILE),
                );
                Ok(WorkflowPathResolution {
                    resolved_workflow_path: path.clone(),
                    dot_path,
                    workflow_config: Some(cfg),
                    workflow_toml_path: Some(path),
                    workflow_slug,
                })
            }
            Err(_)
                if !path.exists() && path.starts_with(crate::run_lookup::default_runs_base()) =>
            {
                let canonical = path.with_file_name(RUN_GRAPH_FILE);
                let legacy = path.with_file_name(LEGACY_RUN_GRAPH_FILE);
                let dot_path = if canonical.exists() || !legacy.exists() {
                    canonical
                } else {
                    legacy
                };
                Ok(WorkflowPathResolution {
                    resolved_workflow_path: path,
                    dot_path,
                    workflow_config: None,
                    workflow_toml_path: None,
                    workflow_slug,
                })
            }
            Err(err) => Err(err),
        }
    } else {
        Ok(WorkflowPathResolution {
            resolved_workflow_path: path.clone(),
            dot_path: path,
            workflow_config: None,
            workflow_toml_path: None,
            workflow_slug,
        })
    }
}

pub fn resolve_settings_for_path(
    workflow_path: &Path,
    defaults: FabroConfig,
    overrides: FabroConfig,
    apply_project_config: bool,
) -> anyhow::Result<FabroSettings> {
    let resolution = resolve_workflow_path(workflow_path)?;
    if resolution.workflow_config.is_none() && !resolution.resolved_workflow_path.is_file() {
        anyhow::bail!(
            "Workflow not found: {}",
            resolution.resolved_workflow_path.display()
        );
    }

    let project_config = if apply_project_config {
        project_config::discover_project_config(
            resolution
                .resolved_workflow_path
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        )?
        .map(|(_, config)| config)
        .unwrap_or_default()
    } else {
        FabroConfig::default()
    };

    overrides
        .combine(resolution.workflow_config.unwrap_or_default())
        .combine(project_config)
        .combine(defaults)
        .try_into()
}

pub fn resolve_workflow(request: ResolveWorkflowRequest) -> anyhow::Result<ResolvedWorkflow> {
    let caller_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match request.workflow {
        WorkflowInput::Path(workflow_path) => {
            let resolution = resolve_workflow_path(&workflow_path)?;
            let settings = request.settings;
            let working_directory = resolve_working_directory(&settings, &caller_cwd);
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
                resolved_workflow_path: Some(resolution.resolved_workflow_path),
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
        WorkflowInput::DotSource {
            source,
            base_dir,
            workflow_slug,
        } => {
            let settings = request.settings;
            let working_directory = resolve_working_directory(&settings, &caller_cwd);
            let goal_override = settings.goal.clone().or(resolve_goal_file(
                settings.goal_file.as_deref(),
                &working_directory,
            )?);
            Ok(ResolvedWorkflow {
                raw_source: source,
                settings,
                workflow_slug,
                workflow_toml_path: None,
                dot_path: None,
                resolved_workflow_path: None,
                base_dir,
                goal_override,
                working_directory,
            })
        }
    }
}
