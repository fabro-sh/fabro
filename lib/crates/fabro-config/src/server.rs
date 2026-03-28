use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use crate::config::FabroConfig;
use crate::settings::{FabroSettings, FabroSettingsExt};
pub use fabro_types::settings::server::{
    ApiAuthStrategy, ApiSettings, AuthProvider, AuthSettings, FeaturesSettings, GitAuthorSettings,
    GitProvider, GitSettings, LogSettings, TlsSettings, WebSettings, WebhookSettings,
    WebhookStrategy,
};

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct AuthConfig {
    pub provider: Option<AuthProvider>,
    #[serde(default)]
    pub allowed_usernames: Vec<String>,
}

impl From<AuthConfig> for AuthSettings {
    fn from(value: AuthConfig) -> Self {
        Self {
            provider: value.provider.unwrap_or_default(),
            allowed_usernames: value.allowed_usernames,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct TlsConfig {
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub ca: Option<PathBuf>,
}

impl TryFrom<TlsConfig> for TlsSettings {
    type Error = anyhow::Error;

    fn try_from(value: TlsConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            cert: value
                .cert
                .ok_or_else(|| anyhow!("tls.cert is required when tls is configured"))?,
            key: value
                .key
                .ok_or_else(|| anyhow!("tls.key is required when tls is configured"))?,
            ca: value
                .ca
                .ok_or_else(|| anyhow!("tls.ca is required when tls is configured"))?,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct ApiConfig {
    pub base_url: Option<String>,
    #[serde(default)]
    pub authentication_strategies: Vec<ApiAuthStrategy>,
    pub tls: Option<TlsConfig>,
}

fn default_base_url() -> String {
    "http://localhost:3000".to_string()
}

impl TryFrom<ApiConfig> for ApiSettings {
    type Error = anyhow::Error;

    fn try_from(value: ApiConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            base_url: value.base_url.unwrap_or_else(default_base_url),
            authentication_strategies: value.authentication_strategies,
            tls: value.tls.map(TryInto::try_into).transpose()?,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct GitAuthorConfig {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl From<GitAuthorConfig> for GitAuthorSettings {
    fn from(value: GitAuthorConfig) -> Self {
        Self {
            name: value.name,
            email: value.email,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct WebhookConfig {
    pub strategy: Option<WebhookStrategy>,
}

impl TryFrom<WebhookConfig> for WebhookSettings {
    type Error = anyhow::Error;

    fn try_from(value: WebhookConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            strategy: value
                .strategy
                .ok_or_else(|| anyhow!("git.webhooks.strategy is required"))?,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct GitConfig {
    pub provider: Option<GitProvider>,
    pub app_id: Option<String>,
    pub client_id: Option<String>,
    pub slug: Option<String>,
    pub author: Option<GitAuthorConfig>,
    pub webhooks: Option<WebhookConfig>,
}

impl TryFrom<GitConfig> for GitSettings {
    type Error = anyhow::Error;

    fn try_from(value: GitConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            provider: value.provider.unwrap_or_default(),
            app_id: value.app_id,
            client_id: value.client_id,
            slug: value.slug,
            author: value.author.map(Into::into).unwrap_or_default(),
            webhooks: value.webhooks.map(TryInto::try_into).transpose()?,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct WebConfig {
    pub url: Option<String>,
    pub auth: Option<AuthConfig>,
}

fn default_web_url() -> String {
    "http://localhost:5173".to_string()
}

impl From<WebConfig> for WebSettings {
    fn from(value: WebConfig) -> Self {
        Self {
            url: value.url.unwrap_or_else(default_web_url),
            auth: value.auth.map(Into::into).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct Features {
    pub session_sandboxes: Option<bool>,
    /// Experimental: enable automatic retro generation after workflow runs.
    pub retros: Option<bool>,
}

impl From<Features> for FeaturesSettings {
    fn from(value: Features) -> Self {
        Self {
            session_sandboxes: value.session_sandboxes.unwrap_or(false),
            retros: value.retros.unwrap_or(false),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct LogConfig {
    pub level: Option<String>,
}

impl From<LogConfig> for LogSettings {
    fn from(value: LogConfig) -> Self {
        Self { level: value.level }
    }
}

/// Load server config from an explicit path or `~/.fabro/server.toml`, returning defaults if the
/// default file doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_server_config(path: Option<&Path>) -> anyhow::Result<FabroConfig> {
    crate::load_config_file(path, "server.toml")
}

pub fn load_server_settings(path: Option<&Path>) -> anyhow::Result<FabroSettings> {
    load_server_config(path)?.try_into()
}

/// Resolve the storage directory: config value > default `~/.fabro`.
pub fn resolve_storage_dir(config: &FabroSettings) -> PathBuf {
    config.storage_dir()
}
