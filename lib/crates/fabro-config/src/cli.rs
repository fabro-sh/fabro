use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize, crate::Combine)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize, crate::Combine)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum PermissionLevel {
    ReadOnly,
    ReadWrite,
    Full,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    #[default]
    Standalone,
    Server,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct ClientTlsConfig {
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub ca: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ClientTlsSettings {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
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
    pub base_url: Option<String>,
    pub tls: Option<ClientTlsConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ServerSettings {
    pub base_url: Option<String>,
    pub tls: Option<ClientTlsSettings>,
}

impl TryFrom<ServerConfig> for ServerSettings {
    type Error = anyhow::Error;

    fn try_from(value: ServerConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            base_url: value.base_url,
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ExecSettings {
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

/// Load CLI config from an explicit path or `~/.fabro/cli.toml`, returning defaults if the
/// default file doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_cli_config(path: Option<&Path>) -> anyhow::Result<crate::config::FabroConfig> {
    crate::load_config_file(path, "cli.toml")
}
