extern crate self as fabro_config;

pub mod combine;
pub mod config;
pub mod dotenv;
pub mod hook;
pub mod mcp;
pub mod project;
pub mod run;
pub mod sandbox;
pub mod server;
pub mod settings;
pub mod user;

pub use config::ConfigLayer;
pub use fabro_types::Combine;
pub use fabro_util::path::expand_tilde;
pub use settings::{FabroSettings, FabroSettingsExt};

use std::path::Path;

use serde::de::DeserializeOwned;

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

    let Some(home) = dirs::home_dir() else {
        tracing::debug!("No home directory found, using default config");
        return Ok(T::default());
    };
    let default_path = home.join(".fabro").join(filename);
    tracing::debug!(path = %default_path.display(), "Loading config");
    match std::fs::read_to_string(&default_path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}
