use std::fmt;

use fabro_types::settings::SettingsLayer;

const CURRENT_VERSION: u32 = 1;

const ALLOWED_TOP_LEVEL_KEYS: &[&str] = &[
    "_version", "project", "workflow", "run", "cli", "server", "features",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Toml(String),
    Version(VersionError),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    LegacyVersionKey,
    UnsupportedHigherVersion { found: u32 },
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LegacyVersionKey => f.write_str(
                "settings files must use `_version` instead of `version`. Rename the key and try again.",
            ),
            Self::UnsupportedHigherVersion { found } => write!(
                f,
                "settings schema version {found} is newer than this build supports (current: {CURRENT_VERSION}). Upgrade Fabro to read this file."
            ),
        }
    }
}

impl std::error::Error for VersionError {}

pub fn parse_settings_layer(input: &str) -> Result<SettingsLayer, ParseError> {
    let raw: toml::Value = toml::from_str(input).map_err(|e| ParseError::Toml(e.to_string()))?;
    validate_version(&raw).map_err(ParseError::Version)?;

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

    raw.try_into::<SettingsLayer>()
        .map_err(|e| ParseError::Toml(e.to_string()))
}

fn validate_version(raw: &toml::Value) -> Result<(), VersionError> {
    if let Some(table) = raw.as_table() {
        if table.contains_key("version") {
            return Err(VersionError::LegacyVersionKey);
        }
        if let Some(value) = table.get("_version").and_then(toml::Value::as_integer) {
            let found = u32::try_from(value).unwrap_or(u32::MAX);
            if found > CURRENT_VERSION {
                return Err(VersionError::UnsupportedHigherVersion { found });
            }
        }
    }
    Ok(())
}

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
        let file = parse_settings_layer("").unwrap();
        assert_eq!(file, SettingsLayer::default());
    }

    #[test]
    fn parses_minimal_valid_file() {
        let file = parse_settings_layer("_version = 1\n").unwrap();
        assert_eq!(file.version, Some(1));
    }

    #[test]
    fn rejects_legacy_version_key_with_rename_hint() {
        let err = parse_settings_layer("version = 1").unwrap_err();
        assert!(matches!(
            err,
            ParseError::Version(VersionError::LegacyVersionKey)
        ));
        assert!(err.to_string().contains("_version"));
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        let err = parse_settings_layer("unknown_key = 1").unwrap_err();
        assert!(matches!(err, ParseError::UnknownTopLevelKey { .. }));
    }

    #[test]
    fn higher_version_rejected_with_upgrade_hint() {
        let err = parse_settings_layer("_version = 99").unwrap_err();
        assert!(err.to_string().contains("Upgrade"));
    }
}
