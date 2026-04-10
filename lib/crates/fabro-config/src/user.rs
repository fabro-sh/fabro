//! User config loading.
//!
//! Exposes machine-level settings loading plus path helpers for the
//! `~/.fabro/settings.toml` file. Runtime types that used to be
//! re-exported from here live in `fabro_types::settings::user` now.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::home::Home;
use crate::load::load_settings_path;
use fabro_types::settings::SettingsFile;

pub const SETTINGS_CONFIG_FILENAME: &str = "settings.toml";
pub const LEGACY_USER_CONFIG_FILENAME: &str = "cli.toml";
pub const LEGACY_OLD_USER_CONFIG_FILENAME: &str = "user.toml";
pub const LEGACY_SERVER_CONFIG_FILENAME: &str = "server.toml";
pub const FABRO_CONFIG_ENV: &str = "FABRO_CONFIG";

static WARNED_LEGACY_USER_CONFIGS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

pub fn default_settings_path() -> PathBuf {
    Home::from_env().user_config()
}

pub fn default_socket_path() -> PathBuf {
    Home::from_env().root().join("fabro.sock")
}

pub fn legacy_default_storage_root() -> PathBuf {
    Home::from_env().root().to_path_buf()
}

pub fn active_settings_path(path: Option<&Path>) -> PathBuf {
    path.map(Path::to_path_buf)
        .or_else(|| std::env::var_os(FABRO_CONFIG_ENV).map(PathBuf::from))
        .unwrap_or_else(default_settings_path)
}

pub fn legacy_user_config_path() -> Option<PathBuf> {
    Some(Home::from_env().root().join(LEGACY_USER_CONFIG_FILENAME))
}

pub fn legacy_old_user_config_path() -> Option<PathBuf> {
    Some(
        Home::from_env()
            .root()
            .join(LEGACY_OLD_USER_CONFIG_FILENAME),
    )
}

pub fn legacy_server_config_path() -> Option<PathBuf> {
    Some(Home::from_env().root().join(LEGACY_SERVER_CONFIG_FILENAME))
}

fn warned_legacy_user_configs() -> &'static Mutex<HashSet<PathBuf>> {
    WARNED_LEGACY_USER_CONFIGS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn should_warn_about_legacy_user_config(path: &Path) -> bool {
    warned_legacy_user_configs()
        .lock()
        .expect("legacy user config warning lock poisoned")
        .insert(path.to_path_buf())
}

/// Load settings config from an explicit path or `~/.fabro/settings.toml`,
/// returning defaults if the default file doesn't exist. An explicit path that
/// doesn't exist is an error.
#[allow(clippy::print_stderr)]
pub fn load_settings_config(path: Option<&Path>) -> anyhow::Result<SettingsFile> {
    if let Some(explicit) = path
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(FABRO_CONFIG_ENV).map(PathBuf::from))
    {
        return load_v2_layer_from_path(&explicit);
    }

    for legacy_path in [
        legacy_user_config_path(),
        legacy_old_user_config_path(),
        legacy_server_config_path(),
    ]
    .into_iter()
    .flatten()
    {
        if legacy_path.is_file() && should_warn_about_legacy_user_config(&legacy_path) {
            let target = default_settings_path();
            eprintln!(
                "Warning: ignoring legacy config file {}. Rename it to {}.",
                legacy_path.display(),
                target.display()
            );
        }
    }

    let default = Home::from_env().root().join(SETTINGS_CONFIG_FILENAME);
    if default.is_file() {
        load_v2_layer_from_path(&default)
    } else {
        Ok(SettingsFile::default())
    }
}

fn load_v2_layer_from_path(path: &Path) -> anyhow::Result<SettingsFile> {
    load_settings_path(path)
}

#[cfg(test)]
mod tests {
    use super::{
        FABRO_CONFIG_ENV, LEGACY_OLD_USER_CONFIG_FILENAME, LEGACY_SERVER_CONFIG_FILENAME,
        LEGACY_USER_CONFIG_FILENAME, SETTINGS_CONFIG_FILENAME, active_settings_path,
        default_settings_path, default_socket_path, legacy_old_user_config_path,
        legacy_server_config_path, legacy_user_config_path, should_warn_about_legacy_user_config,
    };

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&std::path::Path>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn should_warn_about_legacy_user_config_once_per_path() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("cli.toml");
        let second = dir.path().join("other-cli.toml");

        assert!(should_warn_about_legacy_user_config(&first));
        assert!(!should_warn_about_legacy_user_config(&first));
        assert!(should_warn_about_legacy_user_config(&second));
    }

    #[test]
    fn settings_paths_use_expected_filenames() {
        let home = dirs::home_dir().unwrap();

        assert_eq!(
            default_settings_path(),
            home.join(".fabro").join(SETTINGS_CONFIG_FILENAME)
        );
        assert_eq!(default_socket_path(), home.join(".fabro/fabro.sock"));
        assert_eq!(
            legacy_user_config_path(),
            Some(home.join(".fabro").join(LEGACY_USER_CONFIG_FILENAME))
        );
        assert_eq!(
            legacy_old_user_config_path(),
            Some(home.join(".fabro").join(LEGACY_OLD_USER_CONFIG_FILENAME))
        );
        assert_eq!(
            legacy_server_config_path(),
            Some(home.join(".fabro").join(LEGACY_SERVER_CONFIG_FILENAME))
        );
    }

    #[test]
    fn should_warn_once_per_legacy_path_even_with_multiple_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        let server = dir.path().join("server.toml");
        let cli = dir.path().join("cli.toml");

        assert!(should_warn_about_legacy_user_config(&user));
        assert!(!should_warn_about_legacy_user_config(&user));
        assert!(should_warn_about_legacy_user_config(&server));
        assert!(!should_warn_about_legacy_user_config(&server));
        assert!(should_warn_about_legacy_user_config(&cli));
    }

    #[test]
    fn active_settings_path_honors_fabro_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let custom_path = dir.path().join("custom-settings.toml");
        let _guard = EnvGuard::set(FABRO_CONFIG_ENV, Some(&custom_path));

        assert_eq!(active_settings_path(None), custom_path);
    }
}
