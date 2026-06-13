#![expect(
    clippy::disallowed_methods,
    reason = "sync config file load used at startup; not on a Tokio path"
)]

use std::path::{Path, PathBuf};

use fabro_types::settings::InterpString;

use crate::parse::{SettingsSource, validate_settings_source};
use crate::{Error, Result, RunGoalLayer, SettingsLayer, migrations};

#[expect(
    clippy::print_stderr,
    reason = "startup config auto-migration warning must be visible before caller logging is configured"
)]
pub(crate) fn load_settings_path(path: &Path, source: SettingsSource) -> Result<SettingsLayer> {
    let content = std::fs::read_to_string(path).map_err(|source| Error::read_file(path, source))?;
    let content = if source.runs_settings_migrations() {
        match migrations::run_migrations(path, &content)? {
            Some(report) => {
                tracing::warn!("{}", report.warning);
                eprintln!("{}", report.warning);
                report.contents
            }
            None => content,
        }
    } else {
        content
    };
    let mut layer = content
        .parse::<SettingsLayer>()
        .map_err(|err| Error::parse_file("Failed to parse settings file", path, err))?;
    validate_settings_source(&layer, source)
        .map_err(|err| Error::parse_file("Failed to parse settings file", path, err))?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolve_goal_file_paths(&mut layer, base_dir);
    Ok(layer)
}

#[expect(
    clippy::disallowed_methods,
    reason = "guarded by is_literal() above; the raw source is the literal value"
)]
pub(crate) fn resolve_goal_file_paths(file: &mut SettingsLayer, base_dir: &Path) {
    let Some(run) = file.run.as_mut() else {
        return;
    };
    let Some(RunGoalLayer::File { file: goal_file }) = run.goal.as_mut() else {
        return;
    };
    if !goal_file.is_literal() {
        return;
    }
    let literal = goal_file.as_source();
    if Path::new(&literal).is_absolute() {
        return;
    }
    let absolute = resolve_goal_file_path(&literal, base_dir);
    *goal_file = InterpString::parse(&absolute.to_string_lossy());
}

pub(crate) fn resolve_goal_file_path(path_str: &str, base_dir: &Path) -> PathBuf {
    let path = Path::new(path_str);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_settings_path_auto_migrates_legacy_sandbox_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            r#"
_version = 1

[run.sandbox]
provider = "daytona"
"#,
        )
        .expect("write legacy settings");

        let layer = load_settings_path(&path, SettingsSource::ActiveSettings)
            .expect("legacy settings should auto-migrate");

        assert_eq!(
            layer
                .run
                .as_ref()
                .and_then(|run| run.environment.as_ref())
                .and_then(|environment| environment.id.as_deref()),
            Some("daytona")
        );
        assert!(
            std::fs::read_to_string(&path)
                .expect("read rewritten settings")
                .contains("[run.environment]")
        );
        let environment = std::fs::read_to_string(dir.path().join("environments/daytona.toml"))
            .expect("read migrated environment file");
        assert!(environment.contains("provider = \"daytona\""));
    }

    #[test]
    fn load_settings_path_keeps_user_environment_catalog_in_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        let source = r#"
_version = 1

[run.environment]
id = "client"

[environments.client]
provider = "local"
"#;
        std::fs::write(&path, source).expect("write user settings");

        let layer =
            load_settings_path(&path, SettingsSource::User).expect("user settings should load");

        assert!(layer.environments.contains_key("client"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert!(!dir.path().join("environments/client.toml").exists());
    }
}
