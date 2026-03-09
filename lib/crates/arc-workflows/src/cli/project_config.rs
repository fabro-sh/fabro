use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;

const CONFIG_FILENAME: &str = "arc.toml";

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub version: u32,
    #[serde(default)]
    pub arc: ProjectArcConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectArcConfig {
    #[serde(default = "default_root")]
    pub root: String,
    #[serde(default = "default_retro")]
    pub retro: bool,
}

fn default_root() -> String {
    ".".to_string()
}

fn default_retro() -> bool {
    true
}

impl Default for ProjectArcConfig {
    fn default() -> Self {
        Self {
            root: default_root(),
            retro: default_retro(),
        }
    }
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
    tracing::debug!(path = %path.display(), root = %config.arc.root, "Loaded project config");
    Ok(config)
}

/// Walk ancestor directories from `start` looking for `arc.toml`.
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
/// - If the arg has a file extension (`.toml`, `.dot`, etc.), return it as-is.
/// - If no extension, attempt project-based resolution: find `arc.toml`, resolve
///   `{arc_root}/workflows/{name}/workflow.toml`. Falls back to literal arg if
///   no project config or no matching workflow file.
pub fn resolve_workflow_arg(arg: &Path) -> PathBuf {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_workflow_arg_from(arg, &start)
}

fn resolve_workflow_arg_from(arg: &Path, start_dir: &Path) -> PathBuf {
    if arg.extension().is_some() {
        tracing::debug!(arg = %arg.display(), "Workflow arg has extension, returning as-is");
        return arg.to_path_buf();
    }

    let name = arg.to_string_lossy();
    match discover_project_config(start_dir) {
        Ok(Some((config_path, config))) => {
            let arc_root = resolve_arc_root(&config_path, &config);
            let candidate = arc_root
                .join("workflows")
                .join(&*name)
                .join("workflow.toml");
            if candidate.is_file() {
                tracing::debug!(arg = %arg.display(), resolved = %candidate.display(), "Resolved workflow name via project config");
                candidate
            } else {
                tracing::debug!(arg = %arg.display(), candidate = %candidate.display(), "Workflow file not found, falling back to literal");
                arg.to_path_buf()
            }
        }
        Ok(None) => {
            tracing::debug!(arg = %arg.display(), "No project config found, returning literal");
            arg.to_path_buf()
        }
        Err(err) => {
            tracing::debug!(arg = %arg.display(), error = %err, "Error discovering project config, returning literal");
            arg.to_path_buf()
        }
    }
}

/// Check whether retros are enabled in the project config.
/// Returns `true` (the default) if no config is found or on error.
pub fn is_retro_enabled() -> bool {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match discover_project_config(&start) {
        Ok(Some((_path, config))) => config.arc.retro,
        _ => true,
    }
}

/// Resolve the arc root directory from a config file path and its config.
/// The returned path is the directory containing `arc.toml` joined with the `root` value.
pub fn resolve_arc_root(config_path: &Path, config: &ProjectConfig) -> PathBuf {
    let project_dir = config_path
        .parent()
        .expect("config_path should have a parent directory");
    project_dir.join(&config.arc.root)
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
                arc: ProjectArcConfig {
                    root: ".".to_string(),
                    retro: true,
                },
            }
        );
    }

    #[test]
    fn parse_full_config() {
        let config = parse_project_config("version = 1\n[arc]\nroot = \"arc/\"\n").unwrap();
        assert_eq!(config.arc.root, "arc/");
    }

    #[test]
    fn parse_retro_false() {
        let config = parse_project_config("version = 1\n[arc]\nretro = false\n").unwrap();
        assert!(!config.arc.retro);
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
    fn parse_unknown_field_rejected() {
        let err = parse_project_config("version = 1\nfoo = \"bar\"\n").unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("unknown field"), "got: {chain}");
    }

    #[test]
    fn load_from_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("arc.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let config = load_project_config(&path).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.arc.root, ".");
    }

    #[test]
    fn discover_walks_ancestors() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("arc.toml"), "version = 1\n").unwrap();
        let sub = tmp.path().join("sub").join("dir");
        fs::create_dir_all(&sub).unwrap();

        let (found_path, config) = discover_project_config(&sub).unwrap().unwrap();
        assert_eq!(found_path, tmp.path().join("arc.toml"));
        assert_eq!(config.version, 1);
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        let result = discover_project_config(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_arc_root_with_subdirectory() {
        let config_path = Path::new("/repo/arc.toml");
        let config = ProjectConfig {
            version: 1,
            arc: ProjectArcConfig {
                root: "arc/".to_string(),
                ..Default::default()
            },
        };
        assert_eq!(
            resolve_arc_root(config_path, &config),
            Path::new("/repo/arc/")
        );
    }

    #[test]
    fn resolve_arc_root_with_dot() {
        let config_path = Path::new("/repo/arc.toml");
        let config = ProjectConfig {
            version: 1,
            arc: ProjectArcConfig {
                root: ".".to_string(),
                ..Default::default()
            },
        };
        assert_eq!(resolve_arc_root(config_path, &config), Path::new("/repo/."));
    }

    #[test]
    fn resolve_workflow_arg_toml_extension_returned_as_is() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_workflow_arg_from(Path::new("my-workflow.toml"), tmp.path());
        assert_eq!(result, Path::new("my-workflow.toml"));
    }

    #[test]
    fn resolve_workflow_arg_dot_extension_returned_as_is() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_workflow_arg_from(Path::new("my-workflow.dot"), tmp.path());
        assert_eq!(result, Path::new("my-workflow.dot"));
    }

    #[test]
    fn resolve_workflow_arg_no_extension_no_config_returns_literal() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_workflow_arg_from(Path::new("my-workflow"), tmp.path());
        assert_eq!(result, Path::new("my-workflow"));
    }

    #[test]
    fn resolve_workflow_arg_no_extension_with_config_and_workflow_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("arc.toml"), "version = 1\n").unwrap();
        let wf_dir = tmp.path().join("workflows").join("my-workflow");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.dot\"\n",
        )
        .unwrap();

        let result = resolve_workflow_arg_from(Path::new("my-workflow"), tmp.path());
        assert_eq!(result, wf_dir.join("workflow.toml"));
    }

    #[test]
    fn resolve_workflow_arg_no_extension_config_but_no_workflow_dir() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("arc.toml"), "version = 1\n").unwrap();

        let result = resolve_workflow_arg_from(Path::new("my-workflow"), tmp.path());
        assert_eq!(result, Path::new("my-workflow"));
    }

    #[test]
    fn resolve_workflow_arg_custom_root_respected() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("arc.toml"),
            "version = 1\n[arc]\nroot = \"arc/\"\n",
        )
        .unwrap();
        let wf_dir = tmp.path().join("arc").join("workflows").join("factory");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.dot\"\n",
        )
        .unwrap();

        let result = resolve_workflow_arg_from(Path::new("factory"), tmp.path());
        assert_eq!(result, wf_dir.join("workflow.toml"));
    }
}
