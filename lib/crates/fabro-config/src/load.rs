#![expect(
    clippy::disallowed_methods,
    reason = "sync config file load used at startup; not on a Tokio path"
)]

use std::path::{Path, PathBuf};

use fabro_types::settings::InterpString;

use crate::{Error, Result, RunGoalLayer, SettingsLayer, legacy_sandbox_migration};

#[expect(
    clippy::print_stderr,
    reason = "startup config auto-migration warning must be visible before caller logging is configured"
)]
pub(crate) fn load_settings_path(path: &Path) -> Result<SettingsLayer> {
    let content = std::fs::read_to_string(path).map_err(|source| Error::read_file(path, source))?;
    let mut layer = match content.parse::<SettingsLayer>() {
        Ok(layer) => layer,
        Err(err) => match legacy_sandbox_migration::migrate_settings_path(path, &content)? {
            Some(report) => {
                tracing::warn!("{}", report.warning);
                eprintln!("{}", report.warning);
                report.layer
            }
            None => {
                return Err(Error::parse_file(
                    "Failed to parse settings file",
                    path,
                    err,
                ));
            }
        },
    };
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolve_goal_file_paths(&mut layer, base_dir);
    Ok(layer)
}

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
    use fabro_types::settings::run::EnvironmentProvider;

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

        let layer = load_settings_path(&path).expect("legacy settings should auto-migrate");
        let resolved = crate::WorkflowSettingsBuilder::from_layer(&layer)
            .expect("migrated settings should resolve")
            .run;

        assert_eq!(resolved.environment.provider, EnvironmentProvider::Daytona);
        assert!(
            std::fs::read_to_string(&path)
                .expect("read rewritten settings")
                .contains("[run.environment]")
        );
    }
}
