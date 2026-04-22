#![expect(
    clippy::disallowed_methods,
    reason = "sync config loading utilities used at startup; not on a Tokio path"
)]

extern crate self as fabro_config;

mod defaults;

pub mod effective_settings;
pub mod envfile;
pub mod error;
pub mod home;
pub mod load;
pub mod merge;
pub mod parse;
pub mod project;
pub mod resolve;
pub mod run;
pub mod storage;
pub mod user;

use std::path::Path;

pub use defaults::{apply_builtin_defaults, defaults_layer};
pub use error::{Error, Result};
use fabro_types::settings::{Settings, SettingsLayer};
pub use fabro_util::path::expand_tilde;
pub use home::Home;
pub use load::{
    load_settings_for_workflow, load_settings_path, load_settings_project, load_settings_user,
};
pub use parse::{ParseError, parse_settings_layer};
pub use resolve::{
    ResolveError, resolve, resolve_cli, resolve_cli_from_file, resolve_features,
    resolve_features_from_file, resolve_project, resolve_project_from_file, resolve_run,
    resolve_run_from_file, resolve_server, resolve_server_from_file, resolve_workflow,
    resolve_workflow_from_file,
};
use serde::de::DeserializeOwned;
pub use storage::{RunScratch, ServerRuntimeState, Storage};

pub fn load_and_resolve(
    layers: effective_settings::EffectiveSettingsLayers,
    server_settings: Option<&SettingsLayer>,
    mode: effective_settings::EffectiveSettingsMode,
) -> Result<Settings> {
    let layer = effective_settings::materialize_settings_layer(layers, server_settings, mode)?;
    resolve(&layer).map_err(|errors| Error::resolve("failed to resolve settings", errors))
}

/// Load a TOML config from an explicit path or `~/.fabro/{filename}`.
///
/// Returns `T::default()` when no explicit path is given and the default file
/// doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_config_file<T>(path: Option<&Path>, filename: &str) -> Result<T>
where
    T: Default + DeserializeOwned,
{
    if let Some(explicit) = path {
        tracing::debug!(path = %explicit.display(), "Loading config from explicit path");
        let contents = std::fs::read_to_string(explicit)
            .map_err(|source| Error::read_file(explicit, source))?;
        return toml::from_str(&contents).map_err(|source| Error::toml_parse(explicit, source));
    }

    let default_path = Home::from_env().root().join(filename);
    tracing::debug!(path = %default_path.display(), "Loading config");
    match std::fs::read_to_string(&default_path) {
        Ok(contents) => {
            toml::from_str(&contents).map_err(|source| Error::toml_parse(&default_path, source))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::read_file(&default_path, e)),
    }
}
