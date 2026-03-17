use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;

use super::run_config::{
    AssetsConfig, CheckpointConfig, GitHubConfig, LlmConfig, McpServerEntry, PullRequestConfig,
    RunDefaults, SandboxConfig, SetupConfig,
};
use fabro_hooks::HookDefinition;

const CONFIG_FILENAME: &str = "fabro.toml";

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub fabro: ProjectFabroConfig,
    #[serde(default)]
    pub features: ProjectFeatures,
    #[serde(alias = "directory")]
    pub work_dir: Option<String>,
    pub llm: Option<LlmConfig>,
    pub setup: Option<SetupConfig>,
    pub sandbox: Option<SandboxConfig>,
    pub vars: Option<HashMap<String, String>>,
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
    pub pull_request: Option<PullRequestConfig>,
    pub assets: Option<AssetsConfig>,
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerEntry>,
    pub github: Option<GitHubConfig>,
}

impl ProjectConfig {
    /// Convert project config fields into `RunDefaults`.
    pub fn into_run_defaults(self) -> RunDefaults {
        RunDefaults {
            work_dir: self.work_dir,
            llm: self.llm,
            setup: self.setup,
            sandbox: self.sandbox,
            vars: self.vars,
            checkpoint: self.checkpoint,
            pull_request: self.pull_request,
            assets: self.assets,
            hooks: self.hooks,
            mcp_servers: self.mcp_servers,
            github: self.github,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectFabroConfig {
    #[serde(default = "default_root")]
    pub root: String,
}

fn default_root() -> String {
    ".".to_string()
}

impl Default for ProjectFabroConfig {
    fn default() -> Self {
        Self {
            root: default_root(),
        }
    }
}

/// Feature flags for the project. All features default to `false` (opt-in).
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectFeatures {
    /// Experimental: enable automatic retro generation after workflow runs.
    #[serde(default)]
    pub retros: bool,
}

/// Parse a project config from a TOML string.
pub fn parse_project_config(content: &str) -> anyhow::Result<ProjectConfig> {
    let config: ProjectConfig =
        toml::from_str(content).context("Failed to parse project config")?;
    if config.version != 1 {
        bail!(
            "Unsupported project config version: {}. Only version 1 is supported.",
            config.version,
        );
    }
    Ok(config)
}

/// Load a project config from a file path.
pub fn load_project_config(path: &Path) -> anyhow::Result<ProjectConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config = parse_project_config(&content)?;
    tracing::debug!(path = %path.display(), root = %config.fabro.root, "Loaded project config");
    Ok(config)
}

/// Walk ancestor directories from `start` looking for `fabro.toml`.
/// Returns the config file path and parsed config, or `None` if not found.
pub fn discover_project_config(start: &Path) -> anyhow::Result<Option<(PathBuf, ProjectConfig)>> {
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
                msg.push_str(&format!("\n\nDid you mean '{suggestion}'?"));
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
pub struct WorkflowInfo {
    pub name: String,
    pub goal: Option<String>,
    pub source: WorkflowSource,
}

/// Where a workflow was discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
pub fn resolve_workflow(
    arg: &Path,
) -> anyhow::Result<(PathBuf, Option<super::run_config::WorkflowRunConfig>)> {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_workflow_from(arg, &start)
}

fn resolve_workflow_from(
    arg: &Path,
    start_dir: &Path,
) -> anyhow::Result<(PathBuf, Option<super::run_config::WorkflowRunConfig>)> {
    let path = resolve_workflow_arg_from(arg, start_dir)?;
    if path.extension().is_some_and(|ext| ext == "toml") {
        let cfg = super::run_config::load_run_config(&path)?;
        let dot = super::run_config::resolve_graph_path(&path, &cfg.graph);
        Ok((dot, Some(cfg)))
    } else {
        Ok((path, None))
    }
}

/// Check whether retros are enabled in the project config.
/// Returns `false` (the default) if no config is found or on error.
/// Retros are an experimental feature gated behind `[features] retros = true`.
pub fn is_retro_enabled() -> bool {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match discover_project_config(&start) {
        Ok(Some((_path, config))) => config.features.retros,
        _ => false,
    }
}

/// Resolve the fabro root directory from a config file path and its config.
/// The returned path is the directory containing `fabro.toml` joined with the `root` value.
pub fn resolve_fabro_root(config_path: &Path, config: &ProjectConfig) -> PathBuf {
    let project_dir = config_path
        .parent()
        .expect("config_path should have a parent directory");
    project_dir.join(&config.fabro.root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_minimal_config() {
        let config = parse_project_config("version = 1\n").unwrap();
        assert_eq!(
            config,
            ProjectConfig {
                version: 1,
                fabro: ProjectFabroConfig {
                    root: ".".to_string(),
                },
                ..Default::default()
            }
        );
    }

    #[test]
    fn parse_full_config() {
        let config = parse_project_config("version = 1\n[fabro]\nroot = \"fabro/\"\n").unwrap();
        assert_eq!(config.fabro.root, "fabro/");
    }

    #[test]
    fn parse_retros_default_false() {
        let config = parse_project_config("version = 1\n").unwrap();
        assert!(!config.features.retros);
    }

    #[test]
    fn parse_retros_enabled() {
        let config = parse_project_config("version = 1\n[features]\nretros = true\n").unwrap();
        assert!(config.features.retros);
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
                enabled: true,
                draft: false,
                auto_merge: false,
                merge_strategy: crate::cli::run_config::MergeStrategy::Squash,
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
        assert_eq!(snap.name, "my-snapshot");
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
    fn into_run_defaults_preserves_fields() {
        let toml = r#"
version = 1
work_dir = "/ws"
[llm]
model = "m"
[sandbox]
provider = "daytona"
"#;
        let config = parse_project_config(toml).unwrap();
        let defaults = config.into_run_defaults();
        assert_eq!(defaults.work_dir.as_deref(), Some("/ws"));
        assert_eq!(defaults.llm.unwrap().model.as_deref(), Some("m"));
        assert_eq!(
            defaults.sandbox.unwrap().provider.as_deref(),
            Some("daytona")
        );
    }

    #[test]
    fn parse_unknown_field_rejected() {
        let err = parse_project_config("version = 1\nfoo = \"bar\"\n").unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("unknown field"), "got: {chain}");
    }

    #[test]
    fn load_from_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("fabro.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let config = load_project_config(&path).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.fabro.root, ".");
    }

    #[test]
    fn discover_walks_ancestors() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
        let sub = tmp.path().join("sub").join("dir");
        fs::create_dir_all(&sub).unwrap();

        let (found_path, config) = discover_project_config(&sub).unwrap().unwrap();
        assert_eq!(found_path, tmp.path().join("fabro.toml"));
        assert_eq!(config.version, 1);
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
        let config = ProjectConfig {
            version: 1,
            fabro: ProjectFabroConfig {
                root: "fabro/".to_string(),
                ..Default::default()
            },
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
        let config = ProjectConfig {
            version: 1,
            fabro: ProjectFabroConfig {
                root: ".".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            resolve_fabro_root(config_path, &config),
            Path::new("/repo/.")
        );
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
    fn resolve_workflow_arg_unknown_lists_available() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
        let wf_dir = tmp.path().join("workflows").join("hello");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"w.fabro\"\n",
        )
        .unwrap();

        let err = resolve_workflow_arg_from(Path::new("zzzzz"), tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Unknown workflow 'zzzzz'"), "got: {msg}");
        assert!(msg.contains("Available workflows: hello"), "got: {msg}");
        assert!(!msg.contains("Did you mean"), "got: {msg}");
    }

    #[test]
    fn resolve_workflow_arg_no_workflows_dir() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();

        let err = resolve_workflow_arg_from(Path::new("my-workflow"), tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("No workflows found"), "got: {msg}");
    }

    #[test]
    fn resolve_workflow_arg_custom_root_respected() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("fabro.toml"),
            "version = 1\n[fabro]\nroot = \"fabro/\"\n",
        )
        .unwrap();
        let wf_dir = tmp.path().join("fabro").join("workflows").join("factory");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();

        let result = resolve_workflow_arg_from(Path::new("factory"), tmp.path()).unwrap();
        assert_eq!(result, wf_dir.join("workflow.toml"));
    }

    /// Helper: create a workflow dir with workflow.toml + workflow.fabro inside `base/workflows/{name}/`
    fn create_workflow_in(base: &Path, name: &str) {
        let wf_dir = base.join("workflows").join(name);
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        fs::write(
            wf_dir.join("workflow.fabro"),
            "digraph G { start [shape=Mdiamond]; exit [shape=Msquare]; start -> exit }",
        )
        .unwrap();
    }

    /// Helper: create a temp dir with fabro.toml + workflows/{name}/{workflow.toml, workflow.fabro}
    fn setup_workflow_project(name: &str) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
        create_workflow_in(tmp.path(), name);
        let dot_path = tmp
            .path()
            .join("workflows")
            .join(name)
            .join("workflow.fabro");
        (tmp, dot_path)
    }

    #[test]
    fn resolve_workflow_bare_name() {
        let (tmp, expected_dot) = setup_workflow_project("hello");
        let (dot_path, cfg) = resolve_workflow_from(Path::new("hello"), tmp.path()).unwrap();
        assert_eq!(
            dot_path.canonicalize().unwrap(),
            expected_dot.canonicalize().unwrap()
        );
        assert!(cfg.is_some(), "expected Some(RunConfig) for bare name");
    }

    #[test]
    fn resolve_workflow_toml_path() {
        let (tmp, expected_dot) = setup_workflow_project("hello");
        let toml_path = tmp.path().join("workflows/hello/workflow.toml");
        let (dot_path, cfg) = resolve_workflow_from(&toml_path, tmp.path()).unwrap();
        assert_eq!(dot_path, expected_dot);
        assert!(cfg.is_some(), "expected Some(RunConfig) for .toml path");
    }

    #[test]
    fn resolve_workflow_fabro_path() {
        let (tmp, expected_dot) = setup_workflow_project("hello");
        let (dot_path, cfg) = resolve_workflow_from(&expected_dot, tmp.path()).unwrap();
        assert_eq!(dot_path, expected_dot);
        assert!(cfg.is_none(), "expected None for .fabro path");
    }

    #[test]
    fn resolve_workflow_arg_user_workflow_found() {
        let project_dir = TempDir::new().unwrap();
        // No fabro.toml in project_dir
        let user_dir = TempDir::new().unwrap();
        create_workflow_in(user_dir.path(), "my-wf");

        let result = resolve_workflow_arg_impl(
            Path::new("my-wf"),
            project_dir.path(),
            Some(user_dir.path().join("workflows").as_path()),
        )
        .unwrap();
        assert_eq!(
            result,
            user_dir.path().join("workflows/my-wf/workflow.toml")
        );
    }

    #[test]
    fn resolve_workflow_arg_project_takes_precedence() {
        let project_dir = TempDir::new().unwrap();
        fs::write(project_dir.path().join("fabro.toml"), "version = 1\n").unwrap();
        create_workflow_in(project_dir.path(), "shared");

        let user_dir = TempDir::new().unwrap();
        create_workflow_in(user_dir.path(), "shared");

        let result = resolve_workflow_arg_impl(
            Path::new("shared"),
            project_dir.path(),
            Some(user_dir.path().join("workflows").as_path()),
        )
        .unwrap();
        // Should resolve to project, not user
        assert_eq!(
            result,
            project_dir.path().join("workflows/shared/workflow.toml")
        );
    }

    #[test]
    fn resolve_workflow_arg_user_fallback_when_project_missing() {
        let project_dir = TempDir::new().unwrap();
        fs::write(project_dir.path().join("fabro.toml"), "version = 1\n").unwrap();
        // Project has a different workflow
        create_workflow_in(project_dir.path(), "other");

        let user_dir = TempDir::new().unwrap();
        create_workflow_in(user_dir.path(), "my-wf");

        let result = resolve_workflow_arg_impl(
            Path::new("my-wf"),
            project_dir.path(),
            Some(user_dir.path().join("workflows").as_path()),
        )
        .unwrap();
        assert_eq!(
            result,
            user_dir.path().join("workflows/my-wf/workflow.toml")
        );
    }

    #[test]
    fn resolve_workflow_arg_user_workflow_listed_in_error() {
        let project_dir = TempDir::new().unwrap();
        fs::write(project_dir.path().join("fabro.toml"), "version = 1\n").unwrap();
        create_workflow_in(project_dir.path(), "proj-wf");

        let user_dir = TempDir::new().unwrap();
        create_workflow_in(user_dir.path(), "user-wf");

        let err = resolve_workflow_arg_impl(
            Path::new("nonexistent"),
            project_dir.path(),
            Some(user_dir.path().join("workflows").as_path()),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("proj-wf"), "expected proj-wf in: {msg}");
        assert!(msg.contains("user-wf"), "expected user-wf in: {msg}");
    }

    fn create_workflow_with_goal(base: &Path, name: &str, goal: &str) {
        let wf_dir = base.join("workflows").join(name);
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            format!("version = 1\ngoal = \"{goal}\"\ngraph = \"workflow.fabro\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn list_workflows_detailed_project_only() {
        let tmp = TempDir::new().unwrap();
        create_workflow_in(tmp.path(), "alpha");
        create_workflow_with_goal(tmp.path(), "beta", "Run tests");

        let wf_dir = tmp.path().join("workflows");
        let infos = list_workflows_detailed(Some(&wf_dir), None);

        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].name, "alpha");
        assert_eq!(infos[0].goal, None);
        assert_eq!(infos[0].source, WorkflowSource::Project);
        assert_eq!(infos[1].name, "beta");
        assert_eq!(infos[1].goal.as_deref(), Some("Run tests"));
        assert_eq!(infos[1].source, WorkflowSource::Project);
    }

    #[test]
    fn list_workflows_detailed_user_only() {
        let user = TempDir::new().unwrap();
        create_workflow_with_goal(user.path(), "my-wf", "Deploy app");

        let user_wf_dir = user.path().join("workflows");
        let infos = list_workflows_detailed(None, Some(&user_wf_dir));

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "my-wf");
        assert_eq!(infos[0].goal.as_deref(), Some("Deploy app"));
        assert_eq!(infos[0].source, WorkflowSource::User);
    }

    #[test]
    fn list_workflows_detailed_deduplicates_user() {
        let project = TempDir::new().unwrap();
        create_workflow_with_goal(project.path(), "shared", "Project version");

        let user = TempDir::new().unwrap();
        create_workflow_with_goal(user.path(), "shared", "User version");
        create_workflow_in(user.path(), "user-only");

        let project_wf_dir = project.path().join("workflows");
        let user_wf_dir = user.path().join("workflows");
        let infos = list_workflows_detailed(Some(&project_wf_dir), Some(&user_wf_dir));

        assert_eq!(infos.len(), 2);
        let shared = infos.iter().find(|w| w.name == "shared").unwrap();
        assert_eq!(shared.source, WorkflowSource::Project);
        assert_eq!(shared.goal.as_deref(), Some("Project version"));
        let user_only = infos.iter().find(|w| w.name == "user-only").unwrap();
        assert_eq!(user_only.source, WorkflowSource::User);
    }

    #[test]
    fn list_workflows_detailed_sorted() {
        let tmp = TempDir::new().unwrap();
        create_workflow_in(tmp.path(), "zebra");
        create_workflow_in(tmp.path(), "alpha");
        create_workflow_in(tmp.path(), "middle");

        let wf_dir = tmp.path().join("workflows");
        let infos = list_workflows_detailed(Some(&wf_dir), None);
        let names: Vec<_> = infos.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn list_workflows_detailed_empty_dirs() {
        let infos = list_workflows_detailed(None, None);
        assert!(infos.is_empty());
    }

    #[test]
    fn read_workflow_goal_extracts_goal() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("workflow.toml");
        fs::write(
            &path,
            "version = 1\ngoal = \"Hello world\"\ngraph = \"w.fabro\"\n",
        )
        .unwrap();
        assert_eq!(read_workflow_goal(&path).as_deref(), Some("Hello world"));
    }

    #[test]
    fn read_workflow_goal_missing_field() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("workflow.toml");
        fs::write(&path, "version = 1\ngraph = \"w.fabro\"\n").unwrap();
        assert_eq!(read_workflow_goal(&path), None);
    }

    #[test]
    fn read_workflow_goal_missing_file() {
        assert_eq!(
            read_workflow_goal(Path::new("/nonexistent/workflow.toml")),
            None
        );
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

    #[test]
    fn into_run_defaults_preserves_github() {
        let toml = r#"
version = 1

[github]
permissions = { contents = "read", issues = "write" }
"#;
        let config = parse_project_config(toml).unwrap();
        let defaults = config.into_run_defaults();
        let github = defaults.github.unwrap();
        assert_eq!(github.permissions["contents"], "read");
        assert_eq!(github.permissions["issues"], "write");
    }
}
