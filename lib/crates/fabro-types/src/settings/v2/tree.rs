//! The top-level v2 sparse parse tree.
//!
//! This struct models a single settings file (`~/.fabro/settings.toml`,
//! `fabro.toml`, or `workflow.toml`) after parsing. Fields unset in the source
//! stay `None`/empty. Strict unknown-key handling catches any top-level key
//! that is not one of the reserved domains, with targeted rename hints for
//! legacy flat shapes.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::cli::CliLayer;
use super::features::FeaturesLayer;
use super::project::ProjectLayer;
use super::run::RunLayer;
use super::server::ServerLayer;
use super::workflow::WorkflowLayer;

/// A parsed settings file before layering.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsFile {
    #[serde(default, rename = "_version", skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesLayer>,
}

/// A top-level key in a v2 settings file. Anything not in this list is rejected
/// at parse time with a targeted rename hint when possible.
const ALLOWED_TOP_LEVEL_KEYS: &[&str] = &[
    "_version", "project", "workflow", "run", "cli", "server", "features",
];

/// An error returned when a settings file fails parse-level validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A low-level TOML parse error.
    Toml(String),
    /// Schema version pre-validation failed.
    Version(super::version::VersionError),
    /// A top-level key is not part of the v2 schema. Rename hints are
    /// populated for known-legacy keys.
    UnknownTopLevelKey { key: String, hint: Option<String> },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(msg) => write!(f, "settings file is not valid TOML: {msg}"),
            Self::Version(err) => fmt::Display::fmt(err, f),
            Self::UnknownTopLevelKey { key, hint } => {
                if let Some(hint) = hint {
                    write!(f, "unknown top-level settings key `{key}`: {hint}")
                } else {
                    write!(
                        f,
                        "unknown top-level settings key `{key}`: expected one of `_version`, `project`, `workflow`, `run`, `cli`, `server`, `features`"
                    )
                }
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a v2 settings file from TOML text.
///
/// This runs `_version` pre-validation, top-level unknown-key validation
/// with rename hints, and then decodes the sparse namespaced tree. Deeper
/// unknown-key validation for nested tables is enforced by the individual
/// layer types via `#[serde(deny_unknown_fields)]`.
pub fn parse_settings_file(input: &str) -> Result<SettingsFile, ParseError> {
    let raw: toml::Value = toml::from_str(input).map_err(|e| ParseError::Toml(e.to_string()))?;
    super::version::validate_version(&raw).map_err(ParseError::Version)?;

    if let Some(table) = raw.as_table() {
        for key in table.keys() {
            if !ALLOWED_TOP_LEVEL_KEYS.contains(&key.as_str()) {
                return Err(ParseError::UnknownTopLevelKey {
                    key: key.clone(),
                    hint: rename_hint(key),
                });
            }
        }
    }

    let file: SettingsFile = raw
        .try_into::<SettingsFile>()
        .map_err(|e| ParseError::Toml(e.to_string()))?;
    Ok(file)
}

/// Targeted rename hint for known legacy top-level keys.
fn rename_hint(key: &str) -> Option<String> {
    let target = match key {
        "version" => "rename to `_version`",
        "goal" | "goal_file" | "work_dir" | "directory" => "move to `[run]`",
        "graph" => "move to `[workflow]`",
        "labels" => "move to `[run.metadata]`",
        "llm" => "rename to `[run.model]`",
        "vars" => "rename to `[run.inputs]`",
        "setup" => "rename to `[run.prepare]`",
        "sandbox" => "move under `[run.sandbox]`",
        "checkpoint" => "move under `[run.checkpoint]`",
        "pull_request" => "move under `[run.pull_request]`",
        "artifacts" => "move under `[run.artifacts]`",
        "hooks" => "move under `[[run.hooks]]`",
        "mcp_servers" => "move under `[run.agent.mcps.<name>]` or `[cli.exec.agent.mcps.<name>]`",
        "exec" => "rename to `[cli.exec]`",
        "api" => "rename to `[server.api]`",
        "web" => "rename to `[server.web]`",
        "artifact_storage" => "rename to `[server.artifacts]`",
        "storage_dir" | "data_dir" => "rename to `[server.storage] root`",
        "max_concurrent_runs" => "rename to `[server.scheduler]` field",
        "fabro" => "rename to `[project]`; `fabro.root` becomes `project.directory`",
        "git" => "split into `[run.git]` (local git behavior) and `[server.integrations.github]`",
        "github" => "rename to `[server.integrations.github]`",
        "slack" => "move under `[server.integrations.slack]`",
        "log" => "rename to `[server.logging]` or `[cli.logging]` depending on owner",
        "prevent_idle_sleep" => "rename to `[cli.exec] prevent_idle_sleep`",
        "verbose" => "rename to `[cli.output] verbosity`",
        "upgrade_check" => "rename to `[cli.updates] check`",
        "dry_run" => "rename to `[run.execution] mode = \"dry_run\"`",
        "auto_approve" => "rename to `[run.execution] approval = \"auto\"`",
        "no_retro" => "rename to `[run.execution] retros = false`",
        _ => return None,
    };
    Some(target.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_file() {
        let file = parse_settings_file("").unwrap();
        assert_eq!(file, SettingsFile::default());
    }

    #[test]
    fn parses_minimal_valid_file() {
        let input = r#"
_version = 1

[project]
name = "Fabro"
"#;
        let file = parse_settings_file(input).unwrap();
        assert_eq!(file.version, Some(1));
        assert!(file.project.is_some());
    }

    #[test]
    fn rejects_legacy_version_key_with_rename_hint() {
        let err = parse_settings_file("version = 1").unwrap_err();
        assert!(matches!(err, ParseError::Version(_)));
        assert!(err.to_string().contains("_version"));
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        let err = parse_settings_file("unknown_key = 1").unwrap_err();
        let ParseError::UnknownTopLevelKey { key, .. } = err else {
            panic!("expected UnknownTopLevelKey, got: {err:?}");
        };
        assert_eq!(key, "unknown_key");
    }

    #[test]
    fn legacy_llm_section_gets_run_model_rename_hint() {
        let err = parse_settings_file("[llm]\nprovider = \"openai\"").unwrap_err();
        assert!(
            err.to_string().contains("run.model"),
            "expected rename hint for [llm]: {err}"
        );
    }

    #[test]
    fn legacy_vars_section_gets_run_inputs_rename_hint() {
        let err = parse_settings_file("[vars]\nk = \"v\"").unwrap_err();
        assert!(
            err.to_string().contains("run.inputs"),
            "expected rename hint for [vars]: {err}"
        );
    }

    #[test]
    fn legacy_exec_section_gets_cli_exec_rename_hint() {
        let err = parse_settings_file("[exec]\nmodel = \"claude-opus\"").unwrap_err();
        assert!(
            err.to_string().contains("cli.exec"),
            "expected rename hint for [exec]: {err}"
        );
    }

    #[test]
    fn legacy_fabro_section_gets_project_rename_hint() {
        let err = parse_settings_file("[fabro]\nroot = \"fabro/\"").unwrap_err();
        assert!(
            err.to_string().contains("project"),
            "expected rename hint for [fabro]: {err}"
        );
    }

    #[test]
    fn higher_version_rejected_with_upgrade_hint() {
        let err = parse_settings_file("_version = 99").unwrap_err();
        assert!(err.to_string().contains("Upgrade"));
    }
}
