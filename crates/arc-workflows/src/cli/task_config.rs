use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::daytona_env::DaytonaConfig;

const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConfig {
    pub version: u32,
    pub task: String,
    pub graph: String,
    pub directory: Option<String>,
    pub llm: Option<LlmConfig>,
    pub setup: Option<SetupConfig>,
    pub execution: Option<ExecutionConfig>,
    pub vars: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    pub model: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupConfig {
    pub commands: Vec<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    pub environment: Option<String>,
    pub daytona: Option<DaytonaConfig>,
}

/// Load and validate a task config from a TOML file.
///
/// The `graph` path in the returned config is resolved relative to the
/// TOML file's parent directory.
pub fn load_task_config(path: &Path) -> anyhow::Result<TaskConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config = parse_task_config(&contents)?;

    Ok(config)
}

/// Resolve the graph path relative to the TOML file's parent directory.
pub fn resolve_graph_path(toml_path: &Path, graph: &str) -> PathBuf {
    let graph_path = Path::new(graph);
    if graph_path.is_absolute() {
        graph_path.to_path_buf()
    } else {
        toml_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(graph_path)
    }
}

fn parse_task_config(contents: &str) -> anyhow::Result<TaskConfig> {
    let config: TaskConfig =
        toml::from_str(contents).context("Failed to parse task config TOML")?;

    if config.version != SUPPORTED_VERSION {
        bail!(
            "Unsupported task config version {}. Only version {SUPPORTED_VERSION} is supported.",
            config.version
        );
    }

    Ok(config)
}

/// Expand `$name` placeholders in `source` using the given variable map.
///
/// Identifiers match `[a-zA-Z_][a-zA-Z0-9_]*`. A `$` not followed by an
/// identifier character is left as-is. Undefined variables produce an error.
pub fn expand_vars(source: &str, vars: &HashMap<String, String>) -> anyhow::Result<String> {
    let mut result = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'$' {
            let start = i + 1;
            if start < len && (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
                let mut end = start + 1;
                while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                let name = &source[start..end];
                match vars.get(name) {
                    Some(value) => result.push_str(value),
                    None => bail!("Undefined variable: ${name}"),
                }
                i = end;
            } else {
                result.push('$');
                i = start;
            }
        } else {
            result.push(source[i..].chars().next().unwrap());
            i += source[i..].chars().next().unwrap().len_utf8();
        }
    }

    Ok(result)
}

/// Run setup commands sequentially in the given directory.
///
/// Each command gets the full `timeout_ms` budget. Commands are executed
/// via `sh -c` so shell features (pipes, redirects, etc.) work.
pub async fn run_setup(setup: &SetupConfig, directory: &Path) -> anyhow::Result<()> {
    let timeout = std::time::Duration::from_millis(setup.timeout_ms.unwrap_or(300_000));

    for cmd in &setup.commands {
        let fut = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(directory)
            .output();

        let output = tokio::time::timeout(timeout, fut)
            .await
            .with_context(|| {
                format!(
                    "Setup command timed out after {}ms: {cmd}",
                    timeout.as_millis()
                )
            })?
            .with_context(|| format!("Failed to execute setup command: {cmd}"))?;

        if !output.status.success() {
            let code = output
                .status
                .code()
                .map_or("unknown".to_string(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Setup command failed (exit code {code}): {cmd}\n{stderr}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_toml_with_vars() {
        let toml = r#"
version = 1
task = "Run tests"
graph = "pipeline.dot"

[vars]
repo_url = "https://github.com/org/repo"
language = "python"
"#;
        let config = parse_task_config(toml).unwrap();
        let vars = config.vars.unwrap();
        assert_eq!(vars["repo_url"], "https://github.com/org/repo");
        assert_eq!(vars["language"], "python");
    }

    #[test]
    fn expand_single_var() {
        let vars = HashMap::from([("name".to_string(), "world".to_string())]);
        assert_eq!(expand_vars("Hello $name", &vars).unwrap(), "Hello world");
    }

    #[test]
    fn expand_multiple_vars() {
        let vars = HashMap::from([
            ("greeting".to_string(), "Hello".to_string()),
            ("name".to_string(), "world".to_string()),
        ]);
        assert_eq!(
            expand_vars("$greeting $name!", &vars).unwrap(),
            "Hello world!"
        );
    }

    #[test]
    fn expand_undefined_var_errors() {
        let vars = HashMap::new();
        let err = expand_vars("Hello $missing", &vars).unwrap_err();
        assert!(
            err.to_string().contains("Undefined variable: $missing"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn expand_no_vars_passthrough() {
        let vars = HashMap::new();
        assert_eq!(
            expand_vars("no variables here", &vars).unwrap(),
            "no variables here"
        );
    }

    #[test]
    fn expand_dollar_not_followed_by_ident() {
        let vars = HashMap::new();
        assert_eq!(expand_vars("costs $5", &vars).unwrap(), "costs $5");
    }

    #[test]
    fn parse_toml_with_execution() {
        let toml = r#"
version = 1
task = "Run tests"
graph = "pipeline.dot"

[execution]
environment = "daytona"
"#;
        let config = parse_task_config(toml).unwrap();
        let execution = config.execution.unwrap();
        assert_eq!(execution.environment.as_deref(), Some("daytona"));
        assert!(execution.daytona.is_none());
    }

    #[test]
    fn parse_toml_with_daytona_config() {
        let toml = r#"
version = 1
task = "Run tests"
graph = "pipeline.dot"

[execution]
environment = "daytona"

[execution.daytona.sandbox]
auto_stop_interval = 60

[execution.daytona.sandbox.labels]
project = "arc"

[execution.daytona.snapshot]
name = "my-snapshot"
cpu = 4
memory = 8
disk = 10
dockerfile = "FROM rust:1.85-slim-bookworm\nRUN apt-get update"
"#;
        let config = parse_task_config(toml).unwrap();
        let execution = config.execution.unwrap();
        assert_eq!(execution.environment.as_deref(), Some("daytona"));

        let daytona = execution.daytona.unwrap();
        assert_eq!(daytona.sandbox.auto_stop_interval, Some(60));
        let labels = daytona.sandbox.labels.unwrap();
        assert_eq!(labels["project"], "arc");

        let snapshot = daytona.snapshot.unwrap();
        assert_eq!(snapshot.name, "my-snapshot");
        assert_eq!(snapshot.cpu, Some(4));
        assert_eq!(snapshot.memory, Some(8));
        assert_eq!(snapshot.disk, Some(10));
        assert_eq!(
            snapshot.dockerfile.as_deref(),
            Some("FROM rust:1.85-slim-bookworm\nRUN apt-get update")
        );
    }

    #[test]
    fn parse_toml_with_daytona_no_snapshot() {
        let toml = r#"
version = 1
task = "Run tests"
graph = "pipeline.dot"

[execution]
environment = "daytona"

[execution.daytona.sandbox]
auto_stop_interval = 30
"#;
        let config = parse_task_config(toml).unwrap();
        let daytona = config.execution.unwrap().daytona.unwrap();
        assert_eq!(daytona.sandbox.auto_stop_interval, Some(30));
        assert!(daytona.snapshot.is_none());
    }

    #[test]
    fn parse_minimal_toml() {
        let toml = r#"
version = 1
task = "Run tests"
graph = "pipeline.dot"
"#;
        let config = parse_task_config(toml).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.task, "Run tests");
        assert_eq!(config.graph, "pipeline.dot");
        assert!(config.directory.is_none());
        assert!(config.llm.is_none());
        assert!(config.setup.is_none());
    }

    #[test]
    fn parse_full_toml() {
        let toml = r#"
version = 1
task = "Full workflow"
graph = "pipeline.dot"
directory = "/tmp/repo"

[llm]
model = "claude-haiku"
provider = "anthropic"

[setup]
commands = ["pip install -r requirements.txt", "npm install"]
timeout_ms = 60000
"#;
        let config = parse_task_config(toml).unwrap();
        assert_eq!(config.task, "Full workflow");
        assert_eq!(config.directory.as_deref(), Some("/tmp/repo"));

        let llm = config.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));

        let setup = config.setup.unwrap();
        assert_eq!(setup.commands.len(), 2);
        assert_eq!(setup.timeout_ms, Some(60000));
    }

    #[test]
    fn unsupported_version_rejected() {
        let toml = r#"
version = 2
task = "x"
graph = "p.dot"
"#;
        let err = parse_task_config(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported task config version 2"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn graph_path_resolved_relative_to_toml() {
        let toml_path = Path::new("/tmp/sub/task.toml");
        let resolved = resolve_graph_path(toml_path, "p.dot");
        assert_eq!(resolved, PathBuf::from("/tmp/sub/p.dot"));
    }

    #[test]
    fn graph_path_absolute_unchanged() {
        let toml_path = Path::new("/tmp/sub/task.toml");
        let resolved = resolve_graph_path(toml_path, "/other/pipeline.dot");
        assert_eq!(resolved, PathBuf::from("/other/pipeline.dot"));
    }

    #[test]
    fn missing_required_fields() {
        let no_task = r#"
version = 1
graph = "p.dot"
"#;
        assert!(parse_task_config(no_task).is_err());

        let no_graph = r#"
version = 1
task = "x"
"#;
        assert!(parse_task_config(no_graph).is_err());
    }

    #[tokio::test]
    async fn run_setup_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let setup = SetupConfig {
            commands: vec!["echo hello".to_string()],
            timeout_ms: None,
        };
        run_setup(&setup, dir.path()).await.unwrap();
    }

    #[tokio::test]
    async fn run_setup_fails_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let setup = SetupConfig {
            commands: vec!["false".to_string()],
            timeout_ms: None,
        };
        let err = run_setup(&setup, dir.path()).await.unwrap_err();
        assert!(
            err.to_string().contains("exit code"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn run_setup_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let setup = SetupConfig {
            commands: vec!["sleep 10".to_string()],
            timeout_ms: Some(100),
        };
        let err = run_setup(&setup, dir.path()).await.unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
    }
}
