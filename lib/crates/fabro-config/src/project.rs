//! Project-level config loading and workflow discovery.
//!
//! Stage 3 replaced the parse-time `ProjectConfig` type with the v2 parse
//! tree in `fabro_types::settings::v2`. This module keeps the workflow
//! discovery helpers and re-exports resolved project settings.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Serialize;

use crate::config::ConfigLayer;
use crate::run;
use fabro_types::Settings;
pub use fabro_types::settings::project::ProjectSettings;

const CONFIG_FILENAME: &str = "fabro.toml";
const RUN_GRAPH_FILE: &str = "workflow.fabro";
const DEFAULT_FABRO_DIRECTORY: &str = "fabro/";

#[derive(Clone, Debug)]
pub struct WorkflowPathResolution {
    pub resolved_workflow_path: PathBuf,
    pub dot_path: PathBuf,
    pub workflow_config: Option<ConfigLayer>,
    pub workflow_toml_path: Option<PathBuf>,
    pub workflow_slug: Option<String>,
}

/// Parse a project config from a TOML string.
pub fn parse_project_config(content: &str) -> anyhow::Result<ConfigLayer> {
    ConfigLayer::parse(content).context("Failed to parse project config")
}

/// Load a project config from a file path.
pub fn load_project_config(path: &Path) -> anyhow::Result<ConfigLayer> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config = parse_project_config(&content)?;
    let root = config
        .as_v2()
        .project
        .as_ref()
        .and_then(|p| p.directory.as_deref())
        .unwrap_or(DEFAULT_FABRO_DIRECTORY);
    tracing::debug!(path = %path.display(), root = %root, "Loaded project config");
    Ok(config)
}

/// Walk ancestor directories from `start` looking for `fabro.toml`.
/// Returns the config file path and parsed config, or `None` if not found.
pub fn discover_project_config(start: &Path) -> anyhow::Result<Option<(PathBuf, ConfigLayer)>> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(CONFIG_FILENAME);
        if candidate.is_file() {
            tracing::debug!(path = %candidate.display(), "Discovered project config");
            let config = load_project_config(&candidate)?;
            return Ok(Some((candidate, config)));
        }
    }
    Ok(None)
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

/// Resolve a workflow argument to a path.
pub fn resolve_workflow_arg(arg: &Path) -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_workflow_arg_from(arg, &start)
}

pub fn resolve_workflow_path(
    workflow_path: &Path,
    cwd: &Path,
) -> anyhow::Result<WorkflowPathResolution> {
    let path = resolve_workflow_arg_from(workflow_path, cwd)?;
    let workflow_slug = workflow_slug_from_path(&path);
    if path.extension().is_some_and(|ext| ext == "toml") {
        match run::load_run_config(&path) {
            Ok(cfg) => {
                let graph = cfg
                    .as_v2()
                    .workflow
                    .as_ref()
                    .and_then(|w| w.graph.as_deref())
                    .unwrap_or(RUN_GRAPH_FILE);
                let dot_path = run::resolve_graph_path(&path, graph);
                Ok(WorkflowPathResolution {
                    resolved_workflow_path: path.clone(),
                    dot_path,
                    workflow_config: Some(cfg),
                    workflow_toml_path: Some(path),
                    workflow_slug,
                })
            }
            Err(_) if !path.exists() => anyhow::bail!("Workflow not found: {}", path.display()),
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

pub fn resolve_working_directory(settings: &Settings, caller_cwd: &Path) -> PathBuf {
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

fn resolve_workflow_arg_from(arg: &Path, start_dir: &Path) -> anyhow::Result<PathBuf> {
    resolve_workflow_arg_impl(arg, start_dir, Some(&user_workflows_dir()))
}

fn resolve_workflow_arg_impl(
    arg: &Path,
    start_dir: &Path,
    user_workflows: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if arg.extension().is_some() {
        let resolved = if arg.is_absolute() {
            arg.to_path_buf()
        } else {
            start_dir.join(arg)
        };
        tracing::debug!(
            arg = %arg.display(),
            resolved = %resolved.display(),
            "Workflow arg has extension, resolving relative to start dir"
        );
        return Ok(resolved);
    }

    let name = arg.to_string_lossy();
    match discover_project_config(start_dir) {
        Ok(Some((config_path, config))) => {
            let fabro_root = resolve_fabro_root(&config_path, &config);
            let project_candidate = fabro_root
                .join("workflows")
                .join(&*name)
                .join("workflow.toml");
            if project_candidate.is_file() {
                tracing::debug!(arg = %arg.display(), resolved = %project_candidate.display(), "Resolved workflow name via project config");
                return Ok(project_candidate);
            }

            if let Some(resolved) = resolve_user_workflow(user_workflows, &name, arg) {
                return Ok(resolved);
            }

            let project_wf_dir = fabro_root.join("workflows");
            let available = list_available_workflows(Some(&project_wf_dir), user_workflows);
            if available.is_empty() {
                bail!(
                    "Unknown workflow '{name}'\n\nNo workflows found in {}",
                    project_wf_dir.display()
                );
            }
            let mut msg = format!(
                "Unknown workflow '{name}'\n\nAvailable workflows: {}",
                available.join(", ")
            );
            if let Some(suggestion) = find_closest_match(&name, &available) {
                let _ = write!(msg, "\n\nDid you mean '{suggestion}'?");
            }
            bail!("{msg}");
        }
        Ok(None) => {
            if let Some(resolved) = resolve_user_workflow(user_workflows, &name, arg) {
                return Ok(resolved);
            }
            tracing::debug!(arg = %arg.display(), "No project config found, returning literal");
            Ok(arg.to_path_buf())
        }
        Err(err) => {
            tracing::debug!(arg = %arg.display(), error = %err, "Error discovering project config, returning literal");
            Ok(arg.to_path_buf())
        }
    }
}

/// Check if a workflow exists in the user-level workflows directory.
fn resolve_user_workflow(user_workflows: Option<&Path>, name: &str, arg: &Path) -> Option<PathBuf> {
    let user_wf = user_workflows?;
    let candidate = user_wf.join(name).join("workflow.toml");
    if candidate.is_file() {
        tracing::debug!(arg = %arg.display(), resolved = %candidate.display(), "Resolved workflow name via user workflows");
        Some(candidate)
    } else {
        None
    }
}

/// Return the user-level workflows directory (`~/.fabro/workflows/`).
fn user_workflows_dir() -> PathBuf {
    crate::Home::from_env().workflows_dir()
}

/// Metadata about a discovered workflow.
#[derive(Clone, Debug, Serialize)]
pub struct WorkflowInfo {
    pub name: String,
    pub goal: Option<String>,
    pub source: WorkflowSource,
}

/// Where a workflow was discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSource {
    Project,
    User,
}

/// List workflow names in a single directory by scanning for subdirs containing `workflow.toml`.
fn list_workflows_in(workflows_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(workflows_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() && path.join("workflow.toml").is_file() {
                entry.file_name().to_str().map(String::from)
            } else {
                None
            }
        })
        .collect()
}

/// Read the `run.goal` field from a `workflow.toml` without full config validation.
fn read_workflow_goal(workflow_toml: &Path) -> Option<String> {
    let content = std::fs::read_to_string(workflow_toml).ok()?;
    let table: toml::Table = content.parse().ok()?;
    table
        .get("run")?
        .as_table()?
        .get("goal")?
        .as_str()
        .map(String::from)
}

/// List workflows with metadata by scanning project and user workflow directories.
pub fn list_workflows_detailed(
    project_workflows_dir: Option<&Path>,
    user_workflows_dir: Option<&Path>,
) -> Vec<WorkflowInfo> {
    let mut infos: Vec<WorkflowInfo> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    if let Some(dir) = project_workflows_dir {
        for name in list_workflows_in(dir) {
            let goal = read_workflow_goal(&dir.join(&name).join("workflow.toml"));
            seen.push(name.clone());
            infos.push(WorkflowInfo {
                name,
                goal,
                source: WorkflowSource::Project,
            });
        }
    }
    if let Some(dir) = user_workflows_dir {
        for name in list_workflows_in(dir) {
            if !seen.contains(&name) {
                let goal = read_workflow_goal(&dir.join(&name).join("workflow.toml"));
                seen.push(name.clone());
                infos.push(WorkflowInfo {
                    name,
                    goal,
                    source: WorkflowSource::User,
                });
            }
        }
    }

    infos.sort_by(|a, b| a.name.cmp(&b.name));
    infos
}

/// List workflow names by scanning project and user workflow directories.
pub fn list_available_workflows(
    project_workflows_dir: Option<&Path>,
    user_workflows_dir: Option<&Path>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();

    if let Some(dir) = project_workflows_dir {
        names.extend(list_workflows_in(dir));
    }
    if let Some(dir) = user_workflows_dir {
        for name in list_workflows_in(dir) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }

    names.sort();
    names
}

/// Find the closest match using normalized Levenshtein distance (threshold: 0.5).
fn find_closest_match(input: &str, candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .map(|c| (c, strsim::normalized_levenshtein(input, c)))
        .filter(|(_, score)| *score >= 0.5)
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(name, _)| name.clone())
}

/// Resolve a workflow argument to a DOT path and optional run config.
pub fn resolve_workflow(arg: &Path) -> anyhow::Result<(PathBuf, Option<ConfigLayer>)> {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolution = resolve_workflow_path(arg, &start)?;
    Ok((resolution.dot_path, resolution.workflow_config))
}

/// Check whether retros are enabled in the project config.
/// Retros are now expressed as `[run.execution] retros = true` in v2.
pub fn is_retro_enabled() -> bool {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match discover_project_config(&start) {
        Ok(Some((_path, config))) => config
            .as_v2()
            .run
            .as_ref()
            .and_then(|r| r.execution.as_ref())
            .and_then(|e| e.retros)
            .unwrap_or(false),
        _ => false,
    }
}

/// Resolve the fabro root directory from a config file path and its config.
/// The returned path is the directory containing `fabro.toml` joined with the
/// `project.directory` value (default: `fabro/`).
pub fn resolve_fabro_root(config_path: &Path, config: &ConfigLayer) -> PathBuf {
    let project_dir = config_path
        .parent()
        .expect("config_path should have a parent directory");
    let root = config
        .as_v2()
        .project
        .as_ref()
        .and_then(|p| p.directory.as_deref())
        .unwrap_or(DEFAULT_FABRO_DIRECTORY);
    project_dir.join(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_minimal_config() {
        let config = parse_project_config("_version = 1\n").unwrap();
        assert_eq!(config.as_v2().version, Some(1));
        assert!(config.as_v2().project.is_none());
    }

    #[test]
    fn parse_with_project_directory() {
        let config = parse_project_config(
            r#"
_version = 1

[project]
directory = "fabro/"
"#,
        )
        .unwrap();
        assert_eq!(
            config
                .as_v2()
                .project
                .as_ref()
                .and_then(|p| p.directory.as_deref()),
            Some("fabro/")
        );
    }

    #[test]
    fn parse_with_run_execution_retros() {
        let config = parse_project_config(
            r#"
_version = 1

[run.execution]
retros = true
"#,
        )
        .unwrap();
        assert_eq!(
            config
                .as_v2()
                .run
                .as_ref()
                .and_then(|r| r.execution.as_ref())
                .and_then(|e| e.retros),
            Some(true)
        );
    }

    #[test]
    fn parse_rejects_legacy_llm_section() {
        let err = parse_project_config("_version = 1\n[llm]\nprovider = \"openai\"\n").unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("run.model") || text.contains("llm"),
            "expected rename hint for [llm]: {text}"
        );
    }

    #[test]
    fn parse_higher_version_errors() {
        let err = parse_project_config("_version = 2\n").unwrap_err();
        let chain: String = err
            .chain()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            chain.contains("Upgrade") || chain.to_lowercase().contains("version"),
            "Expected version hint in chain: {chain}"
        );
    }

    #[test]
    fn load_from_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("fabro.toml");
        fs::write(&path, "_version = 1\n").unwrap();
        let config = load_project_config(&path).unwrap();
        assert_eq!(config.as_v2().version, Some(1));
    }

    #[test]
    fn discover_walks_ancestors() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "_version = 1\n").unwrap();
        let sub = tmp.path().join("sub").join("dir");
        fs::create_dir_all(&sub).unwrap();

        let (found_path, config) = discover_project_config(&sub).unwrap().unwrap();
        assert_eq!(found_path, tmp.path().join("fabro.toml"));
        assert_eq!(config.as_v2().version, Some(1));
    }
}
