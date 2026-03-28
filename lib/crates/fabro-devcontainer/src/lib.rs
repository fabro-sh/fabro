#![allow(dead_code)]

mod compose;
mod dockerfile;
mod features;
mod jsonc;
mod types;
mod variables;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fabro_util::env::SystemEnv;
pub use types::DevcontainerJson;

/// Lifecycle command — string, array, or object (parallel) form.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Shell(String),
    Args(Vec<String>),
    Parallel(HashMap<String, String>),
}

/// Parsed and resolved devcontainer configuration — everything needed to create a sandbox.
#[derive(Debug, Clone)]
pub struct DevcontainerConfig {
    /// Generated Dockerfile content
    pub dockerfile: String,
    /// Directory for docker build context
    pub build_context: PathBuf,
    /// Build arguments (docker build --build-arg)
    pub build_args: HashMap<String, String>,
    /// Multi-stage build target (docker build --target)
    pub build_target: Option<String>,
    /// Run on host before build
    pub initialize_commands: Vec<Command>,
    /// Run in container after first creation (before updateContentCommand)
    pub on_create_commands: Vec<Command>,
    /// Run in container after creation
    pub post_create_commands: Vec<Command>,
    /// Run in container on each start
    pub post_start_commands: Vec<Command>,
    /// remoteEnv merged
    pub environment: HashMap<String, String>,
    /// containerEnv — baked into Dockerfile as ENV directives
    pub container_env: HashMap<String, String>,
    pub remote_user: Option<String>,
    /// default: /workspaces/{repo-name}
    pub workspace_folder: String,
    /// first = default preview port
    pub forwarded_ports: Vec<u16>,
    /// Compose file paths (empty if not in compose mode)
    pub compose_files: Vec<PathBuf>,
    pub compose_service: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DevcontainerError {
    #[error("no devcontainer.json found in {0}")]
    NotFound(PathBuf),

    #[error("parsing devcontainer.json: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("reading file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("compose file error: {0}")]
    Compose(String),

    #[error("feature error: {0}")]
    Feature(String),

    #[error("oras not found and auto-install failed: {0}")]
    OrasInstall(String),

    #[error("oras command failed: {0}")]
    OrasCommand(String),

    #[error("variable substitution error: {0}")]
    Variable(String),

    #[error(
        "base Dockerfile contains COPY or ADD instructions that reference build context files, which is not supported by Daytona snapshots: {0}"
    )]
    UnsupportedCopyAdd(String),
}

pub type Result<T> = std::result::Result<T, DevcontainerError>;

/// Check that a Dockerfile does not contain COPY or ADD instructions that reference
/// build context files. Multi-stage `COPY --from=` and `ADD http(s)://` are allowed.
fn check_no_build_context_copies(dockerfile: &str) -> Result<()> {
    let mut offending = Vec::new();
    let mut continuation = String::new();

    for raw_line in dockerfile.lines() {
        let trimmed = raw_line.trim();

        // Handle line continuations
        if !continuation.is_empty() {
            continuation.push(' ');
            continuation.push_str(trimmed);
            if trimmed.ends_with('\\') {
                continuation.truncate(continuation.len() - 1);
                continue;
            }
            let full_line = std::mem::take(&mut continuation);
            check_single_line(&full_line, &mut offending);
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.ends_with('\\') {
            continuation = trimmed.trim_end_matches('\\').to_string();
            continue;
        }

        check_single_line(trimmed, &mut offending);
    }

    // Handle unterminated continuation
    if !continuation.is_empty() {
        check_single_line(&continuation, &mut offending);
    }

    if offending.is_empty() {
        Ok(())
    } else {
        Err(DevcontainerError::UnsupportedCopyAdd(offending.join("; ")))
    }
}

fn check_single_line(line: &str, offending: &mut Vec<String>) {
    let upper = line.to_ascii_uppercase();
    if upper.starts_with("COPY ") {
        // Allow COPY --from=<stage>
        let rest = line[5..].trim_start();
        if !rest.starts_with("--from=") && !rest.to_ascii_uppercase().starts_with("--FROM=") {
            offending.push(line.to_string());
        }
    } else if upper.starts_with("ADD ") {
        // Allow ADD http:// or https://
        let rest = line[4..].trim_start();
        if !rest.starts_with("http://") && !rest.starts_with("https://") {
            offending.push(line.to_string());
        }
    }
}

/// Parse and resolve a devcontainer config from a repo directory.
pub struct DevcontainerResolver;

impl DevcontainerResolver {
    /// path: repo root (or explicit .devcontainer/ path)
    pub async fn resolve(path: &Path) -> Result<DevcontainerConfig> {
        let (json_path, devcontainer) = Self::find_and_parse(path)?;
        let repo_root = Self::repo_root_from_json_path(&json_path, path);
        let base_dir = json_path.parent().unwrap_or(path);

        let repo_name = repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
            .to_string();

        // Variable substitution — two-pass: first resolve workspace_folder itself,
        // then create final context with the resolved value.
        let raw_workspace_folder = devcontainer
            .workspace_folder
            .clone()
            .unwrap_or_else(|| format!("/workspaces/{repo_name}"));

        let system_env = SystemEnv;
        let preliminary_vars = variables::VariableContext {
            local_workspace_folder: repo_root.to_string_lossy().to_string(),
            local_workspace_folder_basename: repo_name.clone(),
            container_workspace_folder: raw_workspace_folder.clone(),
            env: &system_env,
        };
        let workspace_folder = variables::substitute(&raw_workspace_folder, &preliminary_vars);

        let vars = variables::VariableContext {
            local_workspace_folder: repo_root.to_string_lossy().to_string(),
            local_workspace_folder_basename: repo_name.clone(),
            container_workspace_folder: workspace_folder.clone(),
            env: &system_env,
        };

        // Handle compose mode
        if let Some(compose_ref) = &devcontainer.docker_compose_file {
            let compose_paths: Vec<PathBuf> = compose_ref
                .paths()
                .iter()
                .map(|p| base_dir.join(variables::substitute(p, &vars)))
                .collect();
            let service_name = devcontainer
                .service
                .as_ref()
                .ok_or_else(|| {
                    DevcontainerError::Compose(
                        "dockerComposeFile requires 'service' field".to_string(),
                    )
                })?
                .clone();

            let compose_config = compose::parse_compose_multi(&compose_paths, &service_name)
                .map_err(DevcontainerError::Compose)?;

            let mut environment = HashMap::new();
            for (k, v) in compose_config.environment {
                environment.insert(k, variables::substitute(&v, &vars));
            }
            if let Some(env) = &devcontainer.remote_env {
                for (k, v) in env {
                    environment.insert(k.clone(), variables::substitute(v, &vars));
                }
            }

            // Use the first compose file's parent as build context base
            let compose_base_dir = compose_paths
                .first()
                .and_then(|p| p.parent())
                .unwrap_or(base_dir);

            let dockerfile = if let Some(build) = &compose_config.build {
                let df_path = compose_base_dir
                    .join(&build.context)
                    .join(build.dockerfile.as_deref().unwrap_or("Dockerfile"));
                std::fs::read_to_string(&df_path).map_err(|source| DevcontainerError::ReadFile {
                    path: df_path,
                    source,
                })?
            } else {
                format!(
                    "FROM {}",
                    compose_config.image.as_deref().unwrap_or("ubuntu")
                )
            };

            check_no_build_context_copies(&dockerfile)?;

            return Ok(DevcontainerConfig {
                dockerfile,
                build_context: compose_base_dir.to_path_buf(),
                build_args: HashMap::new(),
                build_target: None,
                initialize_commands: Self::collect_commands(
                    devcontainer.initialize_command.as_ref(),
                    &vars,
                ),
                on_create_commands: Self::collect_commands(
                    devcontainer.on_create_command.as_ref(),
                    &vars,
                ),
                post_create_commands: Self::collect_commands(
                    devcontainer.post_create_command.as_ref(),
                    &vars,
                ),
                post_start_commands: Self::collect_commands(
                    devcontainer.post_start_command.as_ref(),
                    &vars,
                ),
                environment,
                container_env: Self::collect_container_env(
                    devcontainer.container_env.as_ref(),
                    &vars,
                ),
                remote_user: devcontainer.remote_user.clone().or(compose_config.user),
                workspace_folder,
                forwarded_ports: {
                    let mut ports = compose_config.ports;
                    for port in Self::parse_forward_ports(&devcontainer.forward_ports) {
                        if !ports.contains(&port) {
                            ports.push(port);
                        }
                    }
                    ports
                },
                compose_files: compose_paths,
                compose_service: Some(service_name),
            });
        }

        // Image or Dockerfile mode
        let (base_dockerfile, build_context, build_args, build_target) =
            if let Some(build) = &devcontainer.build {
                let context_dir = build
                    .context
                    .as_ref()
                    .map(|c| base_dir.join(variables::substitute(c, &vars)))
                    .unwrap_or_else(|| base_dir.to_path_buf());
                let df_path = base_dir.join(variables::substitute(
                    build.dockerfile.as_deref().unwrap_or("Dockerfile"),
                    &vars,
                ));
                let content = std::fs::read_to_string(&df_path).map_err(|source| {
                    DevcontainerError::ReadFile {
                        path: df_path,
                        source,
                    }
                })?;
                check_no_build_context_copies(&content)?;
                let args: HashMap<String, String> = build
                    .args
                    .iter()
                    .map(|(k, v)| (k.clone(), variables::substitute(v, &vars)))
                    .collect();
                let target = build
                    .target
                    .as_ref()
                    .map(|t| variables::substitute(t, &vars));
                (content, context_dir, args, target)
            } else {
                let image = devcontainer
                    .image
                    .as_deref()
                    .unwrap_or("mcr.microsoft.com/devcontainers/base:ubuntu");
                (
                    format!("FROM {image}"),
                    base_dir.to_path_buf(),
                    HashMap::new(),
                    None,
                )
            };

        // Features
        let resolved_features = if !devcontainer.features.is_empty() {
            features::resolve_features(
                &devcontainer.features,
                base_dir,
                devcontainer.remote_user.as_deref(),
            )
            .await?
        } else {
            features::ResolvedFeatures::default()
        };

        // Merge feature containerEnv with devcontainer.json containerEnv
        // (devcontainer.json wins on conflicts)
        let mut merged_container_env = resolved_features.container_env;
        if let Some(env) = &devcontainer.container_env {
            for (k, v) in env {
                merged_container_env.insert(k.clone(), variables::substitute(v, &vars));
            }
        }
        // Generate final Dockerfile
        let dockerfile_content = dockerfile::generate(
            &base_dockerfile,
            &resolved_features.layers,
            &merged_container_env,
            devcontainer.remote_user.as_deref(),
        );

        let mut environment = HashMap::new();
        if let Some(env) = &devcontainer.remote_env {
            for (k, v) in env {
                environment.insert(k.clone(), variables::substitute(v, &vars));
            }
        }

        let forwarded_ports = Self::parse_forward_ports(&devcontainer.forward_ports);

        // Collect devcontainer.json lifecycle commands, then append feature lifecycle commands
        let mut on_create_commands =
            Self::collect_commands(devcontainer.on_create_command.as_ref(), &vars);
        let mut post_create_commands =
            Self::collect_commands(devcontainer.post_create_command.as_ref(), &vars);
        let mut post_start_commands =
            Self::collect_commands(devcontainer.post_start_command.as_ref(), &vars);

        for cmd in &resolved_features.on_create_commands {
            on_create_commands.push(Self::convert_lifecycle_command(cmd));
        }
        for cmd in &resolved_features.post_create_commands {
            post_create_commands.push(Self::convert_lifecycle_command(cmd));
        }
        for cmd in &resolved_features.post_start_commands {
            post_start_commands.push(Self::convert_lifecycle_command(cmd));
        }

        Ok(DevcontainerConfig {
            dockerfile: dockerfile_content,
            build_context,
            build_args,
            build_target,
            initialize_commands: Self::collect_commands(
                devcontainer.initialize_command.as_ref(),
                &vars,
            ),
            on_create_commands,
            post_create_commands,
            post_start_commands,
            environment,
            container_env: merged_container_env,
            remote_user: devcontainer.remote_user.clone(),
            workspace_folder,
            forwarded_ports,
            compose_files: Vec::new(),
            compose_service: None,
        })
    }

    fn find_and_parse(path: &Path) -> Result<(PathBuf, DevcontainerJson)> {
        // Check standard locations
        let candidates = [
            path.join(".devcontainer/devcontainer.json"),
            path.join(".devcontainer.json"),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                let raw = std::fs::read_to_string(candidate).map_err(|source| {
                    DevcontainerError::ReadFile {
                        path: candidate.clone(),
                        source,
                    }
                })?;
                let stripped = jsonc::strip_jsonc(&raw);
                let parsed: DevcontainerJson = serde_json::from_str(&stripped)?;
                return Ok((candidate.clone(), parsed));
            }
        }

        // Check if path itself is a devcontainer.json
        if path.is_file() && path.file_name().is_some_and(|n| n == "devcontainer.json") {
            let raw =
                std::fs::read_to_string(path).map_err(|source| DevcontainerError::ReadFile {
                    path: path.to_path_buf(),
                    source,
                })?;
            let stripped = jsonc::strip_jsonc(&raw);
            let parsed: DevcontainerJson = serde_json::from_str(&stripped)?;
            return Ok((path.to_path_buf(), parsed));
        }

        // Subdirectory format: scan .devcontainer/ for subdirs containing devcontainer.json
        let devcontainer_dir = path.join(".devcontainer");
        if devcontainer_dir.is_dir() {
            let mut subdirs: Vec<PathBuf> = std::fs::read_dir(&devcontainer_dir)
                .map_err(|source| DevcontainerError::ReadFile {
                    path: devcontainer_dir.clone(),
                    source,
                })?
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.path())
                .filter(|dir| dir.join("devcontainer.json").exists())
                .collect();

            // Sort alphabetically to get deterministic first pick
            subdirs.sort();

            if let Some(subdir) = subdirs.first() {
                let candidate = subdir.join("devcontainer.json");
                let raw = std::fs::read_to_string(&candidate).map_err(|source| {
                    DevcontainerError::ReadFile {
                        path: candidate.clone(),
                        source,
                    }
                })?;
                let stripped = jsonc::strip_jsonc(&raw);
                let parsed: DevcontainerJson = serde_json::from_str(&stripped)?;
                return Ok((candidate, parsed));
            }
        }

        Err(DevcontainerError::NotFound(path.to_path_buf()))
    }

    fn repo_root_from_json_path<'a>(json_path: &Path, original_path: &'a Path) -> &'a Path {
        // If json_path is inside .devcontainer/<subdir>/, the repo root is two levels up
        // If json_path is inside .devcontainer/, the repo root is one level up
        if let Some(parent) = json_path.parent() {
            if parent.file_name().is_some_and(|n| n == ".devcontainer") {
                if let Some(repo_root) = parent.parent() {
                    let _ = repo_root;
                }
            } else if let Some(grandparent) = parent.parent() {
                if grandparent
                    .file_name()
                    .is_some_and(|n| n == ".devcontainer")
                {
                    if let Some(repo_root) = grandparent.parent() {
                        let _ = repo_root;
                    }
                }
            }
        }
        original_path
    }

    fn collect_container_env(
        env: Option<&HashMap<String, String>>,
        vars: &variables::VariableContext,
    ) -> HashMap<String, String> {
        match env {
            None => HashMap::new(),
            Some(map) => map
                .iter()
                .map(|(k, v)| (k.clone(), variables::substitute(v, vars)))
                .collect(),
        }
    }

    fn convert_lifecycle_command(cmd: &types::LifecycleCommand) -> Command {
        match cmd {
            types::LifecycleCommand::String(s) => Command::Shell(s.clone()),
            types::LifecycleCommand::Array(arr) => Command::Args(arr.clone()),
            types::LifecycleCommand::Object(map) => Command::Parallel(map.clone()),
        }
    }

    fn collect_commands(
        cmd: Option<&types::LifecycleCommand>,
        vars: &variables::VariableContext,
    ) -> Vec<Command> {
        match cmd {
            None => Vec::new(),
            Some(types::LifecycleCommand::String(s)) => {
                vec![Command::Shell(variables::substitute(s, vars))]
            }
            Some(types::LifecycleCommand::Array(arr)) => {
                vec![Command::Args(
                    arr.iter().map(|s| variables::substitute(s, vars)).collect(),
                )]
            }
            Some(types::LifecycleCommand::Object(map)) => {
                vec![Command::Parallel(
                    map.iter()
                        .map(|(k, v)| (k.clone(), variables::substitute(v, vars)))
                        .collect(),
                )]
            }
        }
    }

    fn parse_forward_ports(ports: &[serde_json::Value]) -> Vec<u16> {
        ports
            .iter()
            .filter_map(|p| match p {
                serde_json::Value::Number(n) => n.as_u64().map(|n| n as u16),
                serde_json::Value::String(s) => {
                    let s = s.split('/').next().unwrap_or(s); // strip protocol
                    if let Some((_host, container)) = s.split_once(':') {
                        container.parse::<u16>().ok()
                    } else {
                        s.parse::<u16>().ok()
                    }
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_local_file_is_rejected() {
        let dockerfile = "FROM ubuntu\nCOPY . /app\n";
        let err = check_no_build_context_copies(dockerfile).unwrap_err();
        assert!(
            matches!(err, DevcontainerError::UnsupportedCopyAdd(_)),
            "expected UnsupportedCopyAdd, got: {err:?}"
        );
        assert!(err.to_string().contains("COPY . /app"));
    }

    #[test]
    fn add_local_file_is_rejected() {
        let dockerfile = "FROM ubuntu\nADD local.tar.gz /opt/\n";
        let err = check_no_build_context_copies(dockerfile).unwrap_err();
        assert!(err.to_string().contains("ADD local.tar.gz /opt/"));
    }

    #[test]
    fn copy_from_stage_is_allowed() {
        let dockerfile =
            "FROM builder AS build\nRUN make\nFROM ubuntu\nCOPY --from=builder /app /app\n";
        check_no_build_context_copies(dockerfile).unwrap();
    }

    #[test]
    fn add_url_is_allowed() {
        let dockerfile = "FROM ubuntu\nADD https://example.com/file.tar.gz /opt/\n";
        check_no_build_context_copies(dockerfile).unwrap();
    }

    #[test]
    fn add_http_url_is_allowed() {
        let dockerfile = "FROM ubuntu\nADD http://example.com/file.tar.gz /opt/\n";
        check_no_build_context_copies(dockerfile).unwrap();
    }

    #[test]
    fn only_from_and_run_is_allowed() {
        let dockerfile = "FROM ubuntu\nRUN apt-get update\nENV FOO=bar\n";
        check_no_build_context_copies(dockerfile).unwrap();
    }

    #[test]
    fn multiline_continuation_copy_is_rejected() {
        let dockerfile = "FROM ubuntu\nCOPY \\\n  . /app\n";
        let err = check_no_build_context_copies(dockerfile).unwrap_err();
        assert!(matches!(err, DevcontainerError::UnsupportedCopyAdd(_)));
    }

    #[test]
    fn case_insensitive_copy_is_rejected() {
        let dockerfile = "FROM ubuntu\ncopy . /app\n";
        let err = check_no_build_context_copies(dockerfile).unwrap_err();
        assert!(err.to_string().contains("copy . /app"));
    }

    #[test]
    fn case_insensitive_add_is_rejected() {
        let dockerfile = "FROM ubuntu\nadd local.tar.gz /opt/\n";
        let err = check_no_build_context_copies(dockerfile).unwrap_err();
        assert!(err.to_string().contains("add local.tar.gz /opt/"));
    }

    #[test]
    fn multiple_offending_lines_reported() {
        let dockerfile = "FROM ubuntu\nCOPY . /app\nADD foo.tar /opt/\n";
        let err = check_no_build_context_copies(dockerfile).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("COPY . /app"));
        assert!(msg.contains("ADD foo.tar /opt/"));
    }

    #[test]
    fn comments_and_empty_lines_are_skipped() {
        let dockerfile = "FROM ubuntu\n\n# COPY . /app\n  \nRUN echo hi\n";
        check_no_build_context_copies(dockerfile).unwrap();
    }
}
