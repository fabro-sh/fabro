use std::fmt::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::config::ConfigLayer;
use crate::run;
use fabro_types::Settings;
pub use fabro_types::settings::project::ProjectSettings;

const CONFIG_FILENAME: &str = "fabro.toml";
const SUPPORTED_VERSION: u32 = 1;
const RUN_GRAPH_FILE: &str = "workflow.fabro";
const LEGACY_RUN_GRAPH_FILE: &str = "graph.fabro";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct ProjectConfig {
    pub root: Option<String>,
}

#[derive(Clone, Debug)]
pub struct WorkflowPathResolution {
    pub resolved_workflow_path: PathBuf,
    pub dot_path: PathBuf,
    pub workflow_config: Option<ConfigLayer>,
    pub workflow_toml_path: Option<PathBuf>,
    pub workflow_slug: Option<String>,
}

fn default_root() -> String {
    ".".to_string()
}

impl From<ProjectConfig> for ProjectSettings {
    fn from(value: ProjectConfig) -> Self {
        Self {
            root: value.root.unwrap_or_else(default_root),
        }
    }
}

/// Parse a project config from a TOML string.
pub fn parse_project_config(content: &str) -> anyhow::Result<ConfigLayer> {
    let config: ConfigLayer = toml::from_str(content).context("Failed to parse project config")?;
    let version = config.version.unwrap_or(0);
    if version != SUPPORTED_VERSION {
        bail!(
            "Unsupported project config version: {version}. Only version {SUPPORTED_VERSION} is supported.",
        );
    }
    Ok(config)
}

/// Load a project config from a file path.
pub fn load_project_config(path: &Path) -> anyhow::Result<ConfigLayer> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config = parse_project_config(&content)?;
    let root = config
        .fabro
        .as_ref()
        .and_then(|f| f.root.as_deref())
        .unwrap_or(".");
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

fn cached_workflow_graph_path(path: &Path) -> Option<PathBuf> {
    if path.file_name().and_then(|name| name.to_str()) != Some("workflow.toml") {
        return None;
    }

    let canonical = path.with_file_name(RUN_GRAPH_FILE);
    if canonical.exists() {
        return Some(canonical);
    }

    let legacy = path.with_file_name(LEGACY_RUN_GRAPH_FILE);
    if legacy.exists() {
        return Some(legacy);
    }

    None
}

/// Resolve a workflow argument to a path.
///
/// - If the arg has a file extension (`.toml`, `.fabro`, etc.), return it as-is.
/// - If no extension, attempt project-based resolution: find `fabro.toml`, resolve
///   `{fabro_root}/workflows/{name}/workflow.toml`. Returns an error with suggestions
///   if an `fabro.toml` exists but the workflow wasn't found.
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
                let dot_path =
                    run::resolve_graph_path(&path, cfg.graph.as_deref().unwrap_or(RUN_GRAPH_FILE));
                Ok(WorkflowPathResolution {
                    resolved_workflow_path: path.clone(),
                    dot_path,
                    workflow_config: Some(cfg),
                    workflow_toml_path: Some(path),
                    workflow_slug,
                })
            }
            Err(_) if !path.exists() => {
                let Some(dot_path) = cached_workflow_graph_path(&path) else {
                    anyhow::bail!("Workflow not found: {}", path.display());
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
    resolve_workflow_arg_impl(arg, start_dir, user_workflows_dir().as_deref())
}

fn resolve_workflow_arg_impl(
    arg: &Path,
    start_dir: &Path,
    user_workflows: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if arg.extension().is_some() {
        tracing::debug!(arg = %arg.display(), "Workflow arg has extension, returning as-is");
        return Ok(arg.to_path_buf());
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
fn user_workflows_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".fabro").join("workflows"))
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

/// Read the `goal` field from a `workflow.toml` without full config validation.
fn read_workflow_goal(workflow_toml: &Path) -> Option<String> {
    let content = std::fs::read_to_string(workflow_toml).ok()?;
    let table: toml::Table = content.parse().ok()?;
    table.get("goal")?.as_str().map(String::from)
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
/// Project workflows appear first; user workflows are deduplicated.
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
///
/// Calls `resolve_workflow_arg` first, then if the result is a `.toml` file,
/// loads the run config and resolves the graph path within it.
pub fn resolve_workflow(arg: &Path) -> anyhow::Result<(PathBuf, Option<ConfigLayer>)> {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolution = resolve_workflow_path(arg, &start)?;
    Ok((resolution.dot_path, resolution.workflow_config))
}

/// Check whether retros are enabled in the project config.
/// Returns `false` (the default) if no config is found or on error.
/// Retros are an experimental feature gated behind `[features] retros = true`.
pub fn is_retro_enabled() -> bool {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match discover_project_config(&start) {
        Ok(Some((_path, config))) => config
            .features
            .as_ref()
            .and_then(|f| f.retros)
            .unwrap_or(false),
        _ => false,
    }
}

/// Resolve the fabro root directory from a config file path and its config.
/// The returned path is the directory containing `fabro.toml` joined with the `root` value.
pub fn resolve_fabro_root(config_path: &Path, config: &ConfigLayer) -> PathBuf {
    let project_dir = config_path
        .parent()
        .expect("config_path should have a parent directory");
    let root = config
        .fabro
        .as_ref()
        .and_then(|f| f.root.as_deref())
        .unwrap_or(".");
    project_dir.join(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{LlmConfig, PullRequestConfig};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_minimal_config() {
        let config = parse_project_config("version = 1\n").unwrap();
        assert_eq!(config.version, Some(1));
        assert_eq!(config.fabro, None,);
    }

    #[test]
    fn parse_full_config() {
        let config = parse_project_config("version = 1\n[fabro]\nroot = \"fabro/\"\n").unwrap();
        assert_eq!(config.fabro.unwrap().root.as_deref(), Some("fabro/"));
    }

    #[test]
    fn parse_retros_default_false() {
        let config = parse_project_config("version = 1\n").unwrap();
        assert!(
            !config
                .features
                .as_ref()
                .and_then(|f| f.retros)
                .unwrap_or(false)
        );
    }

    #[test]
    fn parse_retros_enabled() {
        let config = parse_project_config("version = 1\n[features]\nretros = true\n").unwrap();
        assert_eq!(config.features.unwrap().retros, Some(true));
    }

    #[test]
    fn parse_version_mismatch() {
        let err = parse_project_config("version = 2\n").unwrap_err();
        assert!(
            err.to_string().contains("Unsupported"),
            "Expected 'Unsupported' in error, got: {err}"
        );
    }

    #[test]
    fn parse_pull_request_config() {
        let config =
            parse_project_config("version = 1\n\n[pull_request]\nenabled = true\ndraft = false\n")
                .unwrap();
        assert_eq!(
            config.pull_request,
            Some(PullRequestConfig {
                enabled: Some(true),
                draft: Some(false),
                auto_merge: None,
                merge_strategy: None,
            })
        );
    }

    #[test]
    fn parse_project_config_with_sandbox() {
        let toml = r#"
version = 1
[sandbox]
provider = "daytona"
[sandbox.daytona.snapshot]
name = "my-snapshot"
cpu = 4
memory = 8
"#;
        let config = parse_project_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
        let snap = sandbox.daytona.unwrap().snapshot.unwrap();
        assert_eq!(snap.name.as_deref(), Some("my-snapshot"));
        assert_eq!(snap.cpu, Some(4));
        assert_eq!(snap.memory, Some(8));
    }

    #[test]
    fn parse_project_config_with_hooks_and_mcp() {
        let toml = r#"
version = 1
[[hooks]]
event = "run_start"
command = "echo start"
[mcp_servers.playwright]
type = "stdio"
command = ["npx", "@playwright/mcp@latest"]
"#;
        let config = parse_project_config(toml).unwrap();
        assert_eq!(config.hooks.len(), 1);
        assert_eq!(config.mcp_servers.len(), 1);
        assert!(config.mcp_servers.contains_key("playwright"));
    }

    #[test]
    fn parse_project_config_with_llm_and_work_dir() {
        let toml = r#"
version = 1
work_dir = "/workspace"
[llm]
model = "claude-sonnet-4-6"
"#;
        let config = parse_project_config(toml).unwrap();
        assert_eq!(config.work_dir.as_deref(), Some("/workspace"));
        assert_eq!(
            config.llm.unwrap().model.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn load_from_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("fabro.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let config = load_project_config(&path).unwrap();
        assert_eq!(config.version, Some(1));
    }

    #[test]
    fn discover_walks_ancestors() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
        let sub = tmp.path().join("sub").join("dir");
        fs::create_dir_all(&sub).unwrap();

        let (found_path, config) = discover_project_config(&sub).unwrap().unwrap();
        assert_eq!(found_path, tmp.path().join("fabro.toml"));
        assert_eq!(config.version, Some(1));
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        let result = discover_project_config(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_fabro_root_with_subdirectory() {
        let config_path = Path::new("/repo/fabro.toml");
        let config = ConfigLayer {
            version: Some(1),
            fabro: Some(ProjectConfig {
                root: Some("fabro/".to_string()),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_fabro_root(config_path, &config),
            Path::new("/repo/fabro/")
        );
    }

    #[test]
    fn resolve_fabro_root_with_dot() {
        let config_path = Path::new("/repo/fabro.toml");
        let config = ConfigLayer {
            version: Some(1),
            fabro: Some(ProjectConfig {
                root: Some(".".to_string()),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_fabro_root(config_path, &config),
            Path::new("/repo/.")
        );
    }

    #[test]
    fn resolve_fabro_root_without_fabro_section() {
        let config_path = Path::new("/repo/fabro.toml");
        let config = ConfigLayer::default();
        assert_eq!(
            resolve_fabro_root(config_path, &config),
            Path::new("/repo/.")
        );
    }

    #[test]
    fn for_workflow_discovers_project_from_workflow_location() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("project");
        let other_dir = tmp.path().join("other");
        let workflow_dir = project_dir.join("workflows").join("demo");
        fs::create_dir_all(&workflow_dir).unwrap();
        fs::create_dir_all(&other_dir).unwrap();

        fs::write(
            project_dir.join("fabro.toml"),
            "version = 1\nverbose = true\n",
        )
        .unwrap();
        fs::write(
            other_dir.join("fabro.toml"),
            "version = 1\nverbose = false\n",
        )
        .unwrap();
        fs::write(workflow_dir.join("workflow.toml"), "version = 1\n").unwrap();

        let layer =
            ConfigLayer::for_workflow(&workflow_dir.join("workflow.toml"), &other_dir).unwrap();

        assert_eq!(layer.verbose, Some(true));
    }

    #[test]
    fn chained_resolve_preserves_precedence_order() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("project");
        let workflow_dir = project_dir.join("workflows").join("demo");
        fs::create_dir_all(&workflow_dir).unwrap();

        fs::write(
            project_dir.join("fabro.toml"),
            "version = 1\nverbose = true\n[llm]\nmodel = \"project-model\"\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.toml"),
            "version = 1\ndry_run = true\n[llm]\nmodel = \"workflow-model\"\n",
        )
        .unwrap();

        let cli_defaults = ConfigLayer {
            verbose: Some(false),
            llm: Some(LlmConfig {
                model: Some("cli-model".to_string()),
                provider: None,
                fallbacks: None,
            }),
            ..Default::default()
        };
        let overrides = ConfigLayer {
            dry_run: Some(false),
            ..Default::default()
        };

        let settings = overrides
            .combine(
                ConfigLayer::for_workflow(
                    &workflow_dir.join("workflow.toml"),
                    project_dir.as_path(),
                )
                .unwrap(),
            )
            .combine(cli_defaults)
            .resolve()
            .unwrap();

        assert_eq!(
            settings.llm.as_ref().and_then(|llm| llm.model.as_deref()),
            Some("workflow-model")
        );
        assert_eq!(settings.dry_run, Some(false));
        assert_eq!(settings.verbose, Some(true));
    }

    #[test]
    fn resolve_workflow_arg_toml_extension_returned_as_is() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_workflow_arg_from(Path::new("my-workflow.toml"), tmp.path()).unwrap();
        assert_eq!(result, Path::new("my-workflow.toml"));
    }

    #[test]
    fn resolve_workflow_arg_fabro_extension_returned_as_is() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_workflow_arg_from(Path::new("my-workflow.fabro"), tmp.path()).unwrap();
        assert_eq!(result, Path::new("my-workflow.fabro"));
    }

    #[test]
    fn resolve_workflow_arg_no_extension_no_config_returns_literal() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_workflow_arg_from(Path::new("my-workflow"), tmp.path()).unwrap();
        assert_eq!(result, Path::new("my-workflow"));
    }

    #[test]
    fn resolve_workflow_arg_no_extension_with_config_and_workflow_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
        let wf_dir = tmp.path().join("workflows").join("my-workflow");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();

        let result = resolve_workflow_arg_from(Path::new("my-workflow"), tmp.path()).unwrap();
        assert_eq!(result, wf_dir.join("workflow.toml"));
    }

    #[test]
    fn resolve_workflow_arg_typo_suggests_similar_name() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
        let wf_dir = tmp.path().join("workflows").join("implement");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"w.fabro\"\n",
        )
        .unwrap();

        let err = resolve_workflow_arg_from(Path::new("implemet"), tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Unknown workflow 'implemet'"), "got: {msg}");
        assert!(msg.contains("Did you mean 'implement'?"), "got: {msg}");
    }

    #[test]
    fn parse_project_config_with_github() {
        let toml = r#"
version = 1

[github]
permissions = { contents = "read" }
"#;
        let config = parse_project_config(toml).unwrap();
        let github = config.github.unwrap();
        assert_eq!(github.permissions["contents"], "read");
    }
}
