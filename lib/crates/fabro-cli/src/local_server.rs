//! Helpers for CLI code that manages the local Fabro server on this host.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fabro_config::bind::BindRequest;
use fabro_server::serve::resolve_bind_request_from_server_settings;
use fabro_types::ServerSettings;
use fabro_types::settings::ServerAuthMethod;
use fabro_types::settings::server::LogDestination;
use fabro_util::error::SharedError;

use crate::user_config;

pub(crate) struct LocalServerConfig {
    storage_dir:            PathBuf,
    auth_methods:           Vec<ServerAuthMethod>,
    config_log_level:       Option<fabro_config::LogFilter>,
    config_log_destination: Option<LogDestination>,
    server_settings:        std::result::Result<ServerSettings, SharedError>,
}

impl LocalServerConfig {
    pub(crate) fn load(config_path: Option<&Path>, storage_dir: Option<&Path>) -> Result<Self> {
        let settings = user_config::load_resolved_settings(config_path, storage_dir, None)?;
        Ok(Self::from_loaded_settings(settings))
    }

    pub(crate) fn load_with_storage_dir(storage_dir: Option<&Path>) -> Result<Self> {
        let settings = user_config::load_resolved_settings(None, storage_dir, None)?;
        Ok(Self::from_loaded_settings(settings))
    }

    fn from_loaded_settings(settings: user_config::LoadedSettings) -> Self {
        let server_settings = settings.server_settings;
        let auth_methods = server_settings
            .as_ref()
            .map(|resolved| resolved.server.auth.methods.clone())
            .unwrap_or_default();
        Self {
            storage_dir: settings.storage_dir,
            auth_methods,
            config_log_level: settings.config_log_level,
            config_log_destination: settings.config_log_destination,
            server_settings,
        }
    }

    pub(crate) fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    pub(crate) fn auth_methods(&self) -> &[ServerAuthMethod] {
        &self.auth_methods
    }

    pub(crate) fn config_log_level(&self) -> Option<&str> {
        self.config_log_level
            .as_ref()
            .map(fabro_config::LogFilter::as_str)
    }

    pub(crate) fn config_log_destination(&self) -> Option<LogDestination> {
        self.config_log_destination
    }

    pub(crate) fn bind_request(&self, cli_override: Option<&str>) -> Result<BindRequest> {
        let settings = self
            .server_settings
            .as_ref()
            .map_err(|err| anyhow::Error::new(err.clone()))?;
        resolve_bind_request_from_server_settings(settings, cli_override)
    }
}

pub(crate) fn storage_dir_from_toml(source: &str) -> Result<PathBuf> {
    let document: toml::Value = toml::from_str(source).context("failed to parse settings file")?;
    Ok(user_config::storage_dir_from_document(&document, None))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_config::user::default_storage_dir;

    use super::storage_dir_from_toml;

    #[test]
    fn storage_dir_from_toml_reads_explicit_root_without_full_server_resolution() {
        let path = storage_dir_from_toml(
            r#"
_version = 1

[server.storage]
root = "/srv/fabro"
"#,
        )
        .expect("storage root should resolve");

        assert_eq!(path, PathBuf::from("/srv/fabro"));
    }

    #[test]
    fn storage_dir_from_toml_defaults_without_auth_methods() {
        let path = storage_dir_from_toml("_version = 1\n").expect("default storage dir");

        assert_eq!(path, default_storage_dir());
    }

    #[test]
    fn storage_dir_from_toml_keeps_template_token_literal() {
        let path = storage_dir_from_toml(
            r#"
_version = 1

[server.storage]
root = "{{ env.FABRO_STORAGE_ROOT }}"
"#,
        )
        .expect("storage root should parse");

        assert_eq!(path, PathBuf::from("{{ env.FABRO_STORAGE_ROOT }}"));
    }
}
