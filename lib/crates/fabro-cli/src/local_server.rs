//! Helpers for CLI code that manages the local Fabro server on this host.

use std::path::{Path, PathBuf};

use anyhow::Result;
use fabro_config::bind::BindRequest;
use fabro_config::user::default_storage_dir;
use fabro_server::serve::resolve_bind_request_from_server_settings;
use fabro_types::ServerSettings;
use fabro_types::settings::server::LogDestination;
use fabro_types::settings::{InterpString, ServerAuthMethod};

use crate::user_config;

pub(crate) struct LocalServerConfig {
    storage_dir:            PathBuf,
    auth_methods:           Vec<ServerAuthMethod>,
    config_log_level:       Option<fabro_config::LogFilter>,
    config_log_destination: Option<LogDestination>,
    server_settings:        std::result::Result<ServerSettings, String>,
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
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        resolve_bind_request_from_server_settings(settings, cli_override)
    }
}

pub(crate) fn storage_dir_from_toml(source: &str) -> Result<PathBuf> {
    storage_dir_from_toml_with_lookup(source, &process_env_var)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Local server config interpolation owns a process-env lookup facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn storage_dir_from_toml_with_lookup(
    source: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    let document: toml::Value = toml::from_str(source)
        .map_err(|err| anyhow::anyhow!("failed to parse settings file: {err}"))?;
    let storage_root = string_at_path(&document, &["server", "storage", "root"]).map_or_else(
        || InterpString::parse(&default_storage_dir().to_string_lossy()),
        |root| InterpString::parse(&root),
    );
    let resolved_root = storage_root
        .resolve(lookup)
        .map_err(|err| anyhow::anyhow!("failed to resolve {}: {err}", storage_root.as_source()))?;
    Ok(PathBuf::from(resolved_root.value))
}

fn string_at_path(document: &toml::Value, path: &[&str]) -> Option<String> {
    let mut current = document;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_config::user::default_storage_dir;

    use super::{storage_dir_from_toml, storage_dir_from_toml_with_lookup};

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
    fn storage_dir_from_toml_resolves_env_interpolation() {
        let path = storage_dir_from_toml_with_lookup(
            r#"
_version = 1

[server.storage]
root = "{{ env.FABRO_STORAGE_ROOT }}"
"#,
            &|name| (name == "FABRO_STORAGE_ROOT").then_some("/srv/fabro".to_string()),
        )
        .expect("storage root should resolve");

        assert_eq!(path, PathBuf::from("/srv/fabro"));
    }
}
