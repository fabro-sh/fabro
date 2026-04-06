use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use crate::config::ConfigLayer;
use crate::home::Home;

pub use fabro_types::settings::user::{
    ClientTlsSettings, ExecSettings, OutputFormat, PermissionLevel, ServerSettings,
};

pub const SETTINGS_CONFIG_FILENAME: &str = "settings.toml";
pub const LEGACY_USER_CONFIG_FILENAME: &str = "cli.toml";
pub const LEGACY_OLD_USER_CONFIG_FILENAME: &str = "user.toml";
pub const LEGACY_SERVER_CONFIG_FILENAME: &str = "server.toml";

static WARNED_LEGACY_USER_CONFIGS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct ClientTlsConfig {
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub ca: Option<PathBuf>,
}

impl TryFrom<ClientTlsConfig> for ClientTlsSettings {
    type Error = anyhow::Error;

    fn try_from(value: ClientTlsConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            cert: value.cert.ok_or_else(|| {
                anyhow!("server.tls.cert is required when server.tls is configured")
            })?,
            key: value.key.ok_or_else(|| {
                anyhow!("server.tls.key is required when server.tls is configured")
            })?,
            ca: value.ca.ok_or_else(|| {
                anyhow!("server.tls.ca is required when server.tls is configured")
            })?,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct ServerConfig {
    pub target: Option<String>,
    pub tls: Option<ClientTlsConfig>,
}

impl TryFrom<ServerConfig> for ServerSettings {
    type Error = anyhow::Error;

    fn try_from(value: ServerConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            target: value.target,
            tls: value.tls.map(TryInto::try_into).transpose()?,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct ExecConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub permissions: Option<PermissionLevel>,
    pub output_format: Option<OutputFormat>,
}

impl From<ExecConfig> for ExecSettings {
    fn from(value: ExecConfig) -> Self {
        Self {
            provider: value.provider,
            model: value.model,
            permissions: value.permissions,
            output_format: value.output_format,
        }
    }
}

pub fn default_settings_path() -> Option<PathBuf> {
    Some(Home::from_env().user_config())
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

/// Load settings config from an explicit path or `~/.fabro/settings.toml`, returning defaults if the
/// default file doesn't exist. An explicit path that doesn't exist is an error.
#[allow(clippy::print_stderr)]
pub fn load_settings_config(path: Option<&Path>) -> anyhow::Result<ConfigLayer> {
    if let Some(explicit) = path {
        return crate::load_config_file(Some(explicit), SETTINGS_CONFIG_FILENAME);
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
            let target = default_settings_path()
                .unwrap_or_else(|| PathBuf::from(format!("~/.fabro/{SETTINGS_CONFIG_FILENAME}")));
            eprintln!(
                "Warning: ignoring legacy config file {}. Rename it to {}.",
                legacy_path.display(),
                target.display()
            );
        }
    }

    crate::load_config_file(None, SETTINGS_CONFIG_FILENAME)
}

#[cfg(test)]
mod tests {
    use super::{
        LEGACY_OLD_USER_CONFIG_FILENAME, LEGACY_SERVER_CONFIG_FILENAME,
        LEGACY_USER_CONFIG_FILENAME, SETTINGS_CONFIG_FILENAME, default_settings_path,
        legacy_old_user_config_path, legacy_server_config_path, legacy_user_config_path,
        should_warn_about_legacy_user_config,
    };

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
            Some(home.join(".fabro").join(SETTINGS_CONFIG_FILENAME))
        );
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
}
