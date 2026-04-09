extern crate self as fabro_config;

pub mod config;
pub mod effective_settings;
pub mod home;
pub mod legacy_env;
pub mod merge;
pub mod project;
pub mod run;
pub mod storage;
pub mod user;

pub use config::ConfigLayer;
pub use fabro_util::path::expand_tilde;
pub use home::Home;
pub use storage::{RunScratch, ServerState, Storage};

use std::path::{Path, PathBuf};

use fabro_types::settings::v2::SettingsFile;
use serde::de::DeserializeOwned;

/// Resolve the storage directory: v2 `server.storage.root` > home default.
#[must_use]
pub fn resolve_storage_dir(settings: &SettingsFile) -> PathBuf {
    settings.storage_dir()
}

/// Load a TOML config from an explicit path or `~/.fabro/{filename}`.
///
/// Returns `T::default()` when no explicit path is given and the default file
/// doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_config_file<T>(path: Option<&Path>, filename: &str) -> anyhow::Result<T>
where
    T: Default + DeserializeOwned,
{
    if let Some(explicit) = path {
        tracing::debug!(path = %explicit.display(), "Loading config from explicit path");
        let contents = std::fs::read_to_string(explicit)?;
        return Ok(toml::from_str(&contents)?);
    }

    let default_path = Home::from_env().root().join(filename);
    tracing::debug!(path = %default_path.display(), "Loading config");
    match std::fs::read_to_string(&default_path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}
