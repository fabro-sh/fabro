//! User config loading.
//!
//! Exposes machine-level settings loading plus path helpers for the
//! `~/.fabro/settings.toml` file. Runtime types that used to be
//! re-exported from here live in `fabro_types::settings::user` now.

use std::path::{Path, PathBuf};

use crate::home::Home;
use crate::load::load_settings_path;
use crate::{Result, SettingsLayer};

pub const SETTINGS_CONFIG_FILENAME: &str = "settings.toml";
pub const FABRO_CONFIG_ENV: &str = "FABRO_CONFIG";

pub fn default_settings_path() -> PathBuf {
    Home::from_env().user_config()
}

pub fn default_storage_dir() -> PathBuf {
    Home::from_env().root().join("storage")
}

pub fn default_socket_path() -> PathBuf {
    Home::from_env().root().join("fabro.sock")
}

pub fn active_settings_path(path: Option<&Path>) -> PathBuf {
    active_settings_path_with_lookup(path, |name| std::env::var_os(name))
}

fn active_settings_path_with_lookup(
    path: Option<&Path>,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> PathBuf {
    path.map(Path::to_path_buf)
        .or_else(|| lookup(FABRO_CONFIG_ENV).map(PathBuf::from))
        .unwrap_or_else(default_settings_path)
}

/// Load settings config from an explicit path or `~/.fabro/settings.toml`,
/// returning defaults if the default file doesn't exist. An explicit path that
/// doesn't exist is an error.
pub(crate) fn load_settings_config(path: Option<&Path>) -> Result<SettingsLayer> {
    if let Some(explicit) = path
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(FABRO_CONFIG_ENV).map(PathBuf::from))
    {
        return load_v2_layer_from_path(&explicit);
    }

    let default = default_settings_path();
    if default.is_file() {
        load_v2_layer_from_path(&default)
    } else {
        Ok(SettingsLayer::default())
    }
}

fn load_v2_layer_from_path(path: &Path) -> Result<SettingsLayer> {
    load_settings_path(path)
}

#[cfg(test)]
mod tests {
    use super::{
        SETTINGS_CONFIG_FILENAME, active_settings_path_with_lookup, default_settings_path,
        default_socket_path, default_storage_dir,
    };

    #[test]
    fn settings_paths_use_expected_filenames() {
        let home = dirs::home_dir().unwrap();

        assert_eq!(
            default_settings_path(),
            home.join(".fabro").join(SETTINGS_CONFIG_FILENAME)
        );
        assert_eq!(default_storage_dir(), home.join(".fabro/storage"));
        assert_eq!(default_socket_path(), home.join(".fabro/fabro.sock"));
    }

    #[test]
    fn active_settings_path_honors_fabro_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let custom_path = dir.path().join("custom-settings.toml");
        let custom_os = custom_path.clone().into_os_string();
        assert_eq!(
            active_settings_path_with_lookup(None, |_| Some(custom_os.clone())),
            custom_path,
        );
    }
}
