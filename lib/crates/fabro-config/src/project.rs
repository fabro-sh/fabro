//! Project-level config loading and workflow discovery.
//!
//! Stage 3 replaced the parse-time `ProjectConfig` type with the v2 parse
//! tree in `fabro_types::settings::v2`. This module keeps the workflow
//! discovery helpers and re-exports resolved project settings.

#![expect(
    clippy::disallowed_methods,
    reason = "sync project-level config discovery and workflow listing; not on a Tokio path"
)]

use std::fmt::Write;
use std::path::{Path, PathBuf};

use fabro_types::settings::{InterpString, RunNamespace};
use serde::Serialize;

use crate::{Error, Result, WorkflowSettingsBuilder, run};

const CONFIG_FILENAME: &str = ".fabro/project.toml";

/// A workflow's on-disk layout, resolved from any of the user-facing
/// invocation forms (`<name>`, `<dir>/workflow.toml`, `<dir>/workflow.fabro`).
///
/// All three forms converge on the same value object: the workflow directory,
/// the graph file we'll parse, and the run config we'll load settings from
/// (when one is present). Callers don't need to reason about which form the
/// user typed.
#[derive(Clone, Debug)]
pub struct WorkflowLocation {
    /// Directory containing the workflow's files. Always the parent of
    /// `graph`. Used as the anchor for project-config discovery.
    pub dir:   PathBuf,
    /// The `.fabro` (or other graph) file to parse and validate.
    pub graph: PathBuf,
    /// `workflow.toml` providing `[run.*]` settings, when present.
    pub toml:  Option<PathBuf>,
    /// Display name for the workflow (e.g. `"hello"` for
    /// `.fabro/workflows/hello/workflow.fabro`).
    pub slug:  Option<String>,
}

impl WorkflowLocation {
    /// Resolve a user-supplied argument to a workflow location. The argument
    /// may be a workflow name, a path to `workflow.toml`, or a path to a
    /// graph file (e.g. `workflow.fabro`); the three forms produce the same
    /// shape.
    pub fn resolve(arg: &Path, cwd: &Path) -> Result<Self> {
        let resolved = resolve_workflow_arg_from(arg, cwd)?;
        if resolved.extension().is_some_and(|ext| ext == "toml") {
            Self::from_toml(resolved)
        } else {
            Ok(Self::from_graph(resolved))
        }
    }

    fn from_toml(toml_path: PathBuf) -> Result<Self> {
        let cfg = match run::load_run_config(&toml_path) {
            Ok(cfg) => cfg,
            Err(_) if !toml_path.exists() => {
                return Err(Error::WorkflowNotFound(toml_path.display().to_string()));
            }
            Err(err) => return Err(err),
        };
        let workflow = WorkflowSettingsBuilder::workflow_from_layer(&cfg).map_err(|errors| {
            Error::resolve("Failed to resolve workflow settings", errors.into())
        })?;
        let graph = run::resolve_graph_path(&toml_path, &workflow.graph);
        let dir = graph_dir(&graph);
        let slug = workflow_slug_from_path(&toml_path);
        Ok(Self {
            dir,
            graph,
            toml: Some(toml_path),
            slug,
        })
    }

    fn from_graph(graph: PathBuf) -> Self {
        let dir = graph_dir(&graph);
        let toml = sibling_workflow_toml_for(&graph);
        let slug = workflow_slug_from_path(&graph);
        Self {
            dir,
            graph,
            toml,
            slug,
        }
    }
}

/// Walk ancestor directories from `start` looking for `.fabro/project.toml`.
/// Returns the config file path, or `None` if not found.
pub fn discover_project_config(start: &Path) -> Result<Option<PathBuf>> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(CONFIG_FILENAME);
        if candidate.is_file() {
            tracing::debug!(path = %candidate.display(), "Discovered project config");
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn graph_dir(graph: &Path) -> PathBuf {
    graph
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
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

/// Resolve a workflow argument to a path.
pub fn resolve_workflow_arg(arg: &Path) -> Result<PathBuf> {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_workflow_arg_from(arg, &start)
}

/// If `graph` has a sibling `workflow.toml` whose `[workflow].graph` resolves
/// back to `graph`, return the path to the toml. Otherwise return `None`.
/// This is what makes `fabro validate path/to/workflow.fabro` find the inputs
/// defined alongside the graph without the user having to pass the toml.
fn sibling_workflow_toml_for(graph: &Path) -> Option<PathBuf> {
    let candidate = graph.parent()?.join("workflow.toml");
    let cfg = run::load_run_config(&candidate).ok()?;
    let workflow = WorkflowSettingsBuilder::workflow_from_layer(&cfg).ok()?;
    let toml_graph = run::resolve_graph_path(&candidate, &workflow.graph);
    (toml_graph == graph).then_some(candidate)
}

#[expect(
    clippy::disallowed_methods,
    reason = "known leak: run.working_dir is a path kept as InterpString (not demoted); it should \
              resolve {{ env.* }}/{{ vars.* }} tokens but consumes them raw today; strict \
              resolution scheduled in the interpolation unification (Phase 2 keep-rows)"
)]
pub fn resolve_working_directory_from_run(run: &RunNamespace, caller_cwd: &Path) -> PathBuf {
    let Some(work_dir) = run.working_dir.as_ref().map(InterpString::as_source) else {
        return caller_cwd.to_path_buf();
    };
    let path = PathBuf::from(work_dir);
    if path.is_absolute() {
        path
    } else {
        caller_cwd.join(path)
    }
}

fn resolve_workflow_arg_from(arg: &Path, start_dir: &Path) -> Result<PathBuf> {
    resolve_workflow_arg_impl(arg, start_dir, Some(&user_workflows_dir()))
}

fn resolve_workflow_arg_impl(
    arg: &Path,
    start_dir: &Path,
    user_workflows: Option<&Path>,
) -> Result<PathBuf> {
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
        Ok(Some(config_path)) => {
            let fabro_root = config_path
                .parent()
                .expect("project config should have a parent directory");
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
                return Err(Error::other(format!(
                    "Unknown workflow '{name}'\n\nNo workflows found in {}",
                    project_wf_dir.display()
                )));
            }
            let mut msg = format!(
                "Unknown workflow '{name}'\n\nAvailable workflows: {}",
                available.join(", ")
            );
            if let Some(suggestion) = find_closest_match(&name, &available) {
                let _ = write!(msg, "\n\nDid you mean '{suggestion}'?");
            }
            Err(Error::other(msg))
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
    pub name:   String,
    pub goal:   Option<String>,
    pub source: WorkflowSource,
}

/// Where a workflow was discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSource {
    Project,
    User,
}

/// List workflow names in a single directory by scanning for subdirs containing
/// `workflow.toml`.
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

/// Read the `run.goal` field from a `workflow.toml` without full config
/// validation.
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

/// List workflows with metadata by scanning project and user workflow
/// directories.
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

/// Find the closest match using normalized Levenshtein distance (threshold:
/// 0.5).
fn find_closest_match(input: &str, candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .map(|c| (c, strsim::normalized_levenshtein(input, c)))
        .filter(|(_, score)| *score >= 0.5)
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(name, _)| name.clone())
}

/// Resolve a workflow argument to a graph path.
pub fn resolve_workflow(arg: &Path) -> Result<PathBuf> {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(WorkflowLocation::resolve(arg, &start)?.graph)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::tests::workflow_settings_from_toml;

    #[test]
    fn parse_minimal_config() {
        let config = "_version = 1\n".parse::<crate::SettingsLayer>().unwrap();
        assert_eq!(config.version, Some(1));
        assert!(config.project.is_none());
    }

    #[test]
    fn parse_with_project_directory() {
        assert_eq!(
            r#"
_version = 1

[project]
directory = "custom/"
"#
            .parse::<crate::SettingsLayer>()
            .unwrap()
            .project
            .and_then(|project| project.directory),
            Some("custom/".to_string())
        );

        let project = workflow_settings_from_toml(
            r#"
_version = 1

[project]
directory = "custom/"
"#,
        )
        .unwrap()
        .project;
        let json = serde_json::to_value(&project).expect("project settings should serialize");
        assert!(json.get("directory").is_none());
    }

    #[test]
    fn parse_rejects_legacy_llm_section() {
        let err = "_version = 1\n[llm]\nprovider = \"openai\"\n"
            .parse::<crate::SettingsLayer>()
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("run.model") || text.contains("llm"),
            "expected rename hint for [llm]: {text}"
        );
    }

    #[test]
    fn parse_higher_version_errors() {
        let err = "_version = 2\n"
            .parse::<crate::SettingsLayer>()
            .unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("Upgrade") || chain.to_lowercase().contains("version"),
            "Expected version hint in chain: {chain}"
        );
    }

    #[test]
    fn discover_walks_ancestors() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join(".fabro");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("project.toml"), "_version = 1\n").unwrap();
        let sub = tmp.path().join("sub").join("dir");
        fs::create_dir_all(&sub).unwrap();

        let found_path = discover_project_config(&sub).unwrap().unwrap();
        assert_eq!(found_path, config_dir.join("project.toml"));
    }

    #[test]
    fn deprecated_project_directory_does_not_change_fabro_root() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join(".fabro");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("project.toml");
        let workflow_dir = config_dir.join("workflows/demo");
        fs::create_dir_all(&workflow_dir).unwrap();
        fs::write(workflow_dir.join("workflow.toml"), "_version = 1\n").unwrap();
        fs::write(
            &config_path,
            r#"_version = 1

[project]
directory = "../custom"
"#,
        )
        .unwrap();

        assert_eq!(
            resolve_workflow_arg_impl(Path::new("demo"), tmp.path(), None).unwrap(),
            config_dir.join("workflows/demo/workflow.toml")
        );
    }

    #[test]
    fn workflow_location_resolves_bare_fabro_with_sibling_workflow_toml() {
        let tmp = TempDir::new().unwrap();
        let wf_dir = tmp.path().join("wf");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(wf_dir.join("workflow.fabro"), "digraph T {}\n").unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();

        let location =
            WorkflowLocation::resolve(&wf_dir.join("workflow.fabro"), tmp.path()).unwrap();

        assert_eq!(location.dir, wf_dir);
        assert_eq!(location.graph, wf_dir.join("workflow.fabro"));
        assert_eq!(location.toml, Some(wf_dir.join("workflow.toml")));
        assert_eq!(location.slug.as_deref(), Some("wf"));
    }

    #[test]
    fn workflow_location_ignores_sibling_toml_pointing_elsewhere() {
        let tmp = TempDir::new().unwrap();
        let wf_dir = tmp.path().join("wf");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(wf_dir.join("workflow.fabro"), "digraph T {}\n").unwrap();
        fs::write(wf_dir.join("other.fabro"), "digraph T {}\n").unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"other.fabro\"\n",
        )
        .unwrap();

        let location =
            WorkflowLocation::resolve(&wf_dir.join("workflow.fabro"), tmp.path()).unwrap();

        assert_eq!(location.toml, None);
    }

    #[test]
    fn workflow_location_converges_from_name_toml_and_graph_paths() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join(".fabro");
        let wf_dir = config_dir.join("workflows/demo");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(config_dir.join("project.toml"), "_version = 1\n").unwrap();
        fs::write(wf_dir.join("workflow.fabro"), "digraph T {}\n").unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();

        let by_name = WorkflowLocation::resolve(Path::new("demo"), tmp.path()).unwrap();
        let by_toml = WorkflowLocation::resolve(&wf_dir.join("workflow.toml"), tmp.path()).unwrap();
        let by_graph =
            WorkflowLocation::resolve(&wf_dir.join("workflow.fabro"), tmp.path()).unwrap();

        for location in [&by_name, &by_toml, &by_graph] {
            assert_eq!(location.dir, wf_dir);
            assert_eq!(location.graph, wf_dir.join("workflow.fabro"));
            assert_eq!(location.toml, Some(wf_dir.join("workflow.toml")));
            assert_eq!(location.slug.as_deref(), Some("demo"));
        }
    }

    #[test]
    fn resolve_working_directory_from_run_joins_relative_path() {
        let cwd = Path::new("/tmp/workspace");
        let resolved = resolve_working_directory_from_run(
            &RunNamespace {
                working_dir: Some(InterpString::parse("repo")),
                ..RunNamespace::default()
            },
            cwd,
        );

        assert_eq!(resolved, cwd.join("repo"));
    }
}
