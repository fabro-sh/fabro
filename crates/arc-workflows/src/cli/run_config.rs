use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::daytona_sandbox::DaytonaConfig;

const SUPPORTED_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CheckpointConfig {
    #[serde(default)]
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunConfig {
    pub version: u32,
    pub goal: String,
    pub graph: String,
    pub directory: Option<String>,
    pub llm: Option<LlmConfig>,
    pub setup: Option<SetupConfig>,
    pub sandbox: Option<SandboxConfig>,
    pub vars: Option<HashMap<String, String>>,
    #[serde(default)]
    pub hooks: Vec<crate::hook::HookDefinition>,
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LlmConfig {
    pub model: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    pub fallbacks: Option<HashMap<String, Vec<String>>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SetupConfig {
    pub commands: Vec<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SandboxConfig {
    pub provider: Option<String>,
    pub preserve: Option<bool>,
    pub daytona: Option<DaytonaConfig>,
    pub exe: Option<arc_exe::ExeConfig>,
}

/// Defaults for workflow runs, loaded from the server config.
///
/// Fields mirror `WorkflowRunConfig` but are all optional.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RunDefaults {
    pub directory: Option<String>,
    pub llm: Option<LlmConfig>,
    pub setup: Option<SetupConfig>,
    pub sandbox: Option<SandboxConfig>,
    pub vars: Option<HashMap<String, String>>,
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
}

impl WorkflowRunConfig {
    /// Apply server-level run defaults to this config.
    ///
    /// Each field uses the first non-`None` value (task config wins).
    /// Vars are merged: defaults first, then task config overwrites.
    pub fn apply_defaults(&mut self, defaults: &RunDefaults) {
        if self.directory.is_none() {
            self.directory = defaults.directory.clone();
        }

        match (&mut self.llm, &defaults.llm) {
            (Some(task), Some(default)) => {
                if task.model.is_none() {
                    task.model = default.model.clone();
                }
                if task.provider.is_none() {
                    task.provider = default.provider.clone();
                }
                if task.fallbacks.is_none() {
                    task.fallbacks = default.fallbacks.clone();
                }
            }
            (None, Some(_)) => self.llm = defaults.llm.clone(),
            _ => {}
        }

        match (&mut self.setup, &defaults.setup) {
            (Some(task), Some(default)) => {
                if task.timeout_ms.is_none() {
                    task.timeout_ms = default.timeout_ms;
                }
            }
            (None, Some(_)) => self.setup = defaults.setup.clone(),
            _ => {}
        }

        match (&mut self.sandbox, &defaults.sandbox) {
            (Some(task), Some(default)) => {
                if task.provider.is_none() {
                    task.provider = default.provider.clone();
                }
                if task.preserve.is_none() {
                    task.preserve = default.preserve;
                }
                match (&mut task.daytona, &default.daytona) {
                    (Some(task_d), Some(default_d)) => {
                        if task_d.auto_stop_interval.is_none() {
                            task_d.auto_stop_interval = default_d.auto_stop_interval;
                        }
                        if task_d.snapshot.is_none() {
                            task_d.snapshot = default_d.snapshot.clone();
                        }
                        if let Some(ref default_labels) = default_d.labels {
                            let mut merged = default_labels.clone();
                            if let Some(ref task_labels) = task_d.labels {
                                merged.extend(task_labels.clone());
                            }
                            task_d.labels = Some(merged);
                        }
                        if task_d.network.is_none() {
                            task_d.network = default_d.network.clone();
                        }
                    }
                    (None, Some(_)) => task.daytona = default.daytona.clone(),
                    _ => {}
                }
            }
            (None, Some(_)) => self.sandbox = defaults.sandbox.clone(),
            _ => {}
        }

        if let Some(ref default_vars) = defaults.vars {
            let mut merged = default_vars.clone();
            if let Some(ref task_vars) = self.vars {
                merged.extend(task_vars.clone());
            }
            self.vars = Some(merged);
        }

        // Union checkpoint exclude globs from defaults and task config, dedup
        if !defaults.checkpoint.exclude_globs.is_empty() {
            let mut merged = defaults.checkpoint.exclude_globs.clone();
            merged.extend(self.checkpoint.exclude_globs.drain(..));
            merged.sort();
            merged.dedup();
            self.checkpoint.exclude_globs = merged;
        }
    }
}

/// Load and validate a run config from a TOML file.
///
/// The `graph` path in the returned config is resolved relative to the
/// TOML file's parent directory.
pub fn load_run_config(path: &Path) -> anyhow::Result<WorkflowRunConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config = parse_run_config(&contents)?;

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

fn parse_run_config(contents: &str) -> anyhow::Result<WorkflowRunConfig> {
    let config: WorkflowRunConfig =
        toml::from_str(contents).context("Failed to parse run config TOML")?;

    if config.version != SUPPORTED_VERSION {
        bail!(
            "Unsupported run config version {}. Only version {SUPPORTED_VERSION} is supported.",
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
            if start < len && bytes[start] == b'$' {
                result.push('$');
                i = start + 1;
            } else if start < len && (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
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
    use crate::daytona_sandbox::DaytonaSnapshotConfig;

    #[test]
    fn parse_toml_with_vars() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"

[vars]
repo_url = "https://github.com/org/repo"
language = "python"
"#;
        let config = parse_run_config(toml).unwrap();
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
    fn expand_escaped_dollar() {
        let vars = HashMap::from([("name".to_string(), "world".to_string())]);
        assert_eq!(
            expand_vars("literal $$name here", &vars).unwrap(),
            "literal $name here"
        );
    }

    #[test]
    fn expand_escaped_dollar_at_end() {
        let vars = HashMap::new();
        assert_eq!(expand_vars("trailing $$", &vars).unwrap(), "trailing $");
    }

    #[test]
    fn expand_escaped_dollar_before_non_ident() {
        let vars = HashMap::new();
        assert_eq!(expand_vars("price is $$5", &vars).unwrap(), "price is $5");
    }

    #[test]
    fn parse_toml_with_sandbox() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"

[sandbox]
provider = "daytona"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
        assert!(sandbox.daytona.is_none());
    }

    #[test]
    fn parse_toml_with_daytona_config() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"

[sandbox]
provider = "daytona"

[sandbox.daytona]
auto_stop_interval = 60

[sandbox.daytona.labels]
project = "arc"

[sandbox.daytona.snapshot]
name = "my-snapshot"
cpu = 4
memory = 8
disk = 10
dockerfile = "FROM rust:1.85-slim-bookworm\nRUN apt-get update"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));

        let daytona = sandbox.daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(60));
        let labels = daytona.labels.unwrap();
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
goal = "Run tests"
graph = "workflow.dot"

[sandbox]
provider = "daytona"

[sandbox.daytona]
auto_stop_interval = 30
"#;
        let config = parse_run_config(toml).unwrap();
        let daytona = config.sandbox.unwrap().daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(30));
        assert!(daytona.snapshot.is_none());
    }

    #[test]
    fn parse_minimal_toml() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"
"#;
        let config = parse_run_config(toml).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.goal, "Run tests");
        assert_eq!(config.graph, "workflow.dot");
        assert!(config.directory.is_none());
        assert!(config.llm.is_none());
        assert!(config.setup.is_none());
    }

    #[test]
    fn parse_full_toml() {
        let toml = r#"
version = 1
goal = "Full workflow"
graph = "workflow.dot"
directory = "/tmp/repo"

[llm]
model = "claude-haiku"
provider = "anthropic"

[setup]
commands = ["pip install -r requirements.txt", "npm install"]
timeout_ms = 60000
"#;
        let config = parse_run_config(toml).unwrap();
        assert_eq!(config.goal, "Full workflow");
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
goal = "x"
graph = "p.dot"
"#;
        let err = parse_run_config(toml).unwrap_err();
        assert!(
            err.to_string().contains("Unsupported run config version 2"),
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
        let resolved = resolve_graph_path(toml_path, "/other/workflow.dot");
        assert_eq!(resolved, PathBuf::from("/other/workflow.dot"));
    }

    #[test]
    fn missing_required_fields() {
        let no_goal = r#"
version = 1
graph = "p.dot"
"#;
        assert!(parse_run_config(no_goal).is_err());

        let no_graph = r#"
version = 1
goal = "x"
"#;
        assert!(parse_run_config(no_graph).is_err());
    }

    #[test]
    fn parse_run_defaults_with_llm() {
        let toml = r#"
[llm]
model = "claude-haiku"
provider = "anthropic"
"#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        let llm = defaults.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn parse_run_defaults_empty() {
        let defaults: RunDefaults = toml::from_str("").unwrap();
        assert!(defaults.directory.is_none());
        assert!(defaults.llm.is_none());
        assert!(defaults.setup.is_none());
        assert!(defaults.sandbox.is_none());
        assert!(defaults.vars.is_none());
    }

    #[test]
    fn parse_run_defaults_full() {
        let toml = r#"
directory = "/work"

[llm]
model = "gpt-4"
provider = "openai"

[setup]
commands = ["make build"]
timeout_ms = 5000

[sandbox]
provider = "daytona"

[vars]
key = "value"
"#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        assert_eq!(defaults.directory.as_deref(), Some("/work"));
        assert!(defaults.llm.is_some());
        assert!(defaults.setup.is_some());
        assert!(defaults.sandbox.is_some());
        assert_eq!(defaults.vars.as_ref().unwrap()["key"], "value");
    }

    #[test]
    fn apply_defaults_fills_missing_llm() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: Some("default-model".into()),
                provider: Some("anthropic".into()),
                fallbacks: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("default-model"));
    }

    #[test]
    fn apply_defaults_task_config_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[llm]
model = "task-model"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: Some("default-model".into()),
                provider: None,
                fallbacks: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("task-model"));
    }

    #[test]
    fn apply_defaults_merges_vars() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[vars]
task_key = "task_val"
shared = "from_task"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            vars: Some(HashMap::from([
                ("default_key".into(), "default_val".into()),
                ("shared".into(), "from_default".into()),
            ])),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let vars = cfg.vars.unwrap();
        assert_eq!(vars["default_key"], "default_val");
        assert_eq!(vars["task_key"], "task_val");
        // Task config wins on collision
        assert_eq!(vars["shared"], "from_task");
    }

    #[test]
    fn apply_defaults_vars_default_only() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            vars: Some(HashMap::from([("key".into(), "val".into())])),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let vars = cfg.vars.unwrap();
        assert_eq!(vars["key"], "val");
    }

    #[test]
    fn apply_defaults_merges_llm_fields() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[llm]
model = "haiku"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: None,
                provider: Some("anthropic".into()),
                fallbacks: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn apply_defaults_merges_setup_timeout() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[setup]
commands = ["make test"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            setup: Some(SetupConfig {
                commands: vec!["make build".into()],
                timeout_ms: Some(60000),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let setup = cfg.setup.unwrap();
        assert_eq!(setup.commands, vec!["make test"]);
        assert_eq!(setup.timeout_ms, Some(60000));
    }

    #[test]
    fn parse_toml_with_sandbox_preserve() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"

[sandbox]
provider = "docker"
preserve = true
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.preserve, Some(true));
    }

    #[test]
    fn parse_toml_sandbox_preserve_defaults_to_none() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"

[sandbox]
provider = "docker"
"#;
        let config = parse_run_config(toml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.preserve, None);
    }

    #[test]
    fn apply_defaults_merges_sandbox_preserve_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox]
preserve = true
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: Some(false),
                daytona: None,
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.sandbox.unwrap().preserve, Some(true));
    }

    #[test]
    fn apply_defaults_merges_sandbox_preserve_from_default() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox]
provider = "docker"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: Some(true),
                daytona: None,
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(cfg.sandbox.unwrap().preserve, Some(true));
    }

    #[test]
    fn apply_defaults_merges_sandbox_fields() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox]
provider = "daytona"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                daytona: Some(DaytonaConfig {
                    auto_stop_interval: Some(30),
                    ..DaytonaConfig::default()
                }),
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let sandbox = cfg.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
        let daytona = sandbox.daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(30));
    }

    #[test]
    fn apply_defaults_merges_daytona_fields() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox]
provider = "daytona"

[sandbox.daytona]
auto_stop_interval = 60
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: Some("daytona".into()),
                preserve: None,
                daytona: Some(DaytonaConfig {
                    auto_stop_interval: Some(30),
                    labels: Some(HashMap::from([("env".into(), "prod".into())])),
                    ..DaytonaConfig::default()
                }),
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let daytona = cfg.sandbox.unwrap().daytona.unwrap();
        assert_eq!(daytona.auto_stop_interval, Some(60));
        assert_eq!(daytona.labels.as_ref().unwrap()["env"], "prod");
    }

    #[test]
    fn apply_defaults_merges_daytona_labels() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox.daytona.labels]
project = "arc"
env = "from_task"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                daytona: Some(DaytonaConfig {
                    labels: Some(HashMap::from([
                        ("env".into(), "from_default".into()),
                        ("team".into(), "platform".into()),
                    ])),
                    ..DaytonaConfig::default()
                }),
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let labels = cfg.sandbox.unwrap().daytona.unwrap().labels.unwrap();
        assert_eq!(labels["project"], "arc");
        assert_eq!(labels["team"], "platform");
        assert_eq!(labels["env"], "from_task");
    }

    #[test]
    fn apply_defaults_daytona_snapshot_whole_struct() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox.daytona.snapshot]
name = "task-snap"
cpu = 2
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                daytona: Some(DaytonaConfig {
                    snapshot: Some(DaytonaSnapshotConfig {
                        name: "default-snap".into(),
                        cpu: Some(8),
                        memory: Some(16),
                        disk: Some(100),
                        dockerfile: Some("FROM ubuntu".into()),
                    }),
                    ..DaytonaConfig::default()
                }),
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let snapshot = cfg.sandbox.unwrap().daytona.unwrap().snapshot.unwrap();
        assert_eq!(snapshot.name, "task-snap");
        assert_eq!(snapshot.cpu, Some(2));
        assert!(snapshot.memory.is_none());
    }

    #[test]
    fn apply_defaults_daytona_snapshot_from_default() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox.daytona]
auto_stop_interval = 60
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                daytona: Some(DaytonaConfig {
                    snapshot: Some(DaytonaSnapshotConfig {
                        name: "default-snap".into(),
                        cpu: Some(4),
                        memory: Some(8),
                        disk: None,
                        dockerfile: None,
                    }),
                    ..DaytonaConfig::default()
                }),
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let snapshot = cfg.sandbox.unwrap().daytona.unwrap().snapshot.unwrap();
        assert_eq!(snapshot.name, "default-snap");
        assert_eq!(snapshot.cpu, Some(4));
        assert_eq!(snapshot.memory, Some(8));
    }

    #[test]
    fn parse_toml_with_fallbacks() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"

[llm]
model = "claude-opus-4-6"
provider = "anthropic"

[llm.fallbacks]
anthropic = ["gemini", "openai"]
gemini = ["anthropic", "openai"]
"#;
        let config = parse_run_config(toml).unwrap();
        let llm = config.llm.unwrap();
        let fallbacks = llm.fallbacks.unwrap();
        assert_eq!(fallbacks["anthropic"], vec!["gemini", "openai"]);
        assert_eq!(fallbacks["gemini"], vec!["anthropic", "openai"]);
    }

    #[test]
    fn parse_toml_without_fallbacks() {
        let toml = r#"
version = 1
goal = "Run tests"
graph = "workflow.dot"

[llm]
model = "claude-opus-4-6"
provider = "anthropic"
"#;
        let config = parse_run_config(toml).unwrap();
        let llm = config.llm.unwrap();
        assert!(llm.fallbacks.is_none());
    }

    #[test]
    fn apply_defaults_fallbacks_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[llm]
model = "opus"

[llm.fallbacks]
anthropic = ["gemini"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: None,
                provider: Some("anthropic".into()),
                fallbacks: Some(HashMap::from([(
                    "anthropic".into(),
                    vec!["openai".into()],
                )])),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(
            llm.fallbacks.unwrap()["anthropic"],
            vec!["gemini"]
        );
    }

    #[test]
    fn apply_defaults_fallbacks_inherited() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[llm]
model = "opus"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            llm: Some(LlmConfig {
                model: None,
                provider: Some("anthropic".into()),
                fallbacks: Some(HashMap::from([(
                    "anthropic".into(),
                    vec!["openai".into()],
                )])),
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        let llm = cfg.llm.unwrap();
        assert_eq!(
            llm.fallbacks.unwrap()["anthropic"],
            vec!["openai"]
        );
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

    #[test]
    fn parse_toml_with_hooks() {
        let toml = r#"
version = 1
goal = "Test hooks"
graph = "test.dot"

[[hooks]]
event = "stage_start"
command = "./scripts/pre-check.sh"
blocking = true
sandbox = false

[[hooks]]
event = "run_complete"
command = "echo done"
"#;
        let cfg: WorkflowRunConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.hooks.len(), 2);
        assert_eq!(cfg.hooks[0].event, crate::hook::HookEvent::StageStart);
        assert_eq!(
            cfg.hooks[0].command.as_deref(),
            Some("./scripts/pre-check.sh")
        );
        assert_eq!(cfg.hooks[0].blocking, Some(true));
        assert_eq!(cfg.hooks[0].sandbox, Some(false));
        assert_eq!(cfg.hooks[1].event, crate::hook::HookEvent::RunComplete);
    }

    #[test]
    fn parse_toml_without_hooks_defaults_empty() {
        let toml = r#"
version = 1
goal = "No hooks"
graph = "test.dot"
"#;
        let cfg: WorkflowRunConfig = toml::from_str(toml).unwrap();
        assert!(cfg.hooks.is_empty());
    }

    #[test]
    fn parse_toml_with_daytona_network_block() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox.daytona]
network = "block"
"#;
        let config = parse_run_config(toml).unwrap();
        let daytona = config.sandbox.unwrap().daytona.unwrap();
        assert_eq!(
            daytona.network,
            Some(crate::daytona_sandbox::DaytonaNetwork::Block)
        );
    }

    #[test]
    fn apply_defaults_network_task_wins() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox.daytona]
network = "block"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                daytona: Some(DaytonaConfig {
                    network: Some(crate::daytona_sandbox::DaytonaNetwork::AllowAll),
                    ..DaytonaConfig::default()
                }),
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.sandbox.unwrap().daytona.unwrap().network,
            Some(crate::daytona_sandbox::DaytonaNetwork::Block)
        );
    }

    #[test]
    fn apply_defaults_network_inherited() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[sandbox.daytona]
auto_stop_interval = 60
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            sandbox: Some(SandboxConfig {
                provider: None,
                preserve: None,
                daytona: Some(DaytonaConfig {
                    network: Some(crate::daytona_sandbox::DaytonaNetwork::AllowList(vec![
                        "10.0.0.0/8".into(),
                    ])),
                    ..DaytonaConfig::default()
                }),
                exe: None,
            }),
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.sandbox.unwrap().daytona.unwrap().network,
            Some(crate::daytona_sandbox::DaytonaNetwork::AllowList(vec![
                "10.0.0.0/8".into(),
            ]))
        );
    }

    #[test]
    fn parse_toml_with_checkpoint_exclude_globs() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.dot"

[checkpoint]
exclude_globs = ["**/node_modules/**", "**/.cache/**"]
"#;
        let config = parse_run_config(toml).unwrap();
        assert_eq!(
            config.checkpoint.exclude_globs,
            vec!["**/node_modules/**", "**/.cache/**"]
        );
    }

    #[test]
    fn parse_toml_without_checkpoint_defaults_empty() {
        let toml = r#"
version = 1
goal = "test"
graph = "w.dot"
"#;
        let config = parse_run_config(toml).unwrap();
        assert!(config.checkpoint.exclude_globs.is_empty());
    }

    #[test]
    fn apply_defaults_unions_checkpoint_exclude_globs() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[checkpoint]
exclude_globs = ["**/dist/**", "**/.cache/**"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            checkpoint: CheckpointConfig {
                exclude_globs: vec![
                    "**/.cache/**".into(),
                    "**/node_modules/**".into(),
                ],
            },
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.checkpoint.exclude_globs,
            vec!["**/.cache/**", "**/dist/**", "**/node_modules/**"]
        );
    }

    #[test]
    fn apply_defaults_checkpoint_from_defaults_only() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"
"#,
        )
        .unwrap();
        let defaults = RunDefaults {
            checkpoint: CheckpointConfig {
                exclude_globs: vec!["**/node_modules/**".into()],
            },
            ..RunDefaults::default()
        };
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.checkpoint.exclude_globs,
            vec!["**/node_modules/**"]
        );
    }

    #[test]
    fn apply_defaults_checkpoint_from_task_only() {
        let mut cfg = parse_run_config(
            r#"
version = 1
goal = "test"
graph = "w.dot"

[checkpoint]
exclude_globs = ["**/dist/**"]
"#,
        )
        .unwrap();
        let defaults = RunDefaults::default();
        cfg.apply_defaults(&defaults);
        assert_eq!(
            cfg.checkpoint.exclude_globs,
            vec!["**/dist/**"]
        );
    }

    #[test]
    fn parse_run_defaults_with_checkpoint() {
        let toml = r#"
[checkpoint]
exclude_globs = ["**/node_modules/**"]
"#;
        let defaults: RunDefaults = toml::from_str(toml).unwrap();
        assert_eq!(
            defaults.checkpoint.exclude_globs,
            vec!["**/node_modules/**"]
        );
    }
}
