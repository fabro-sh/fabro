//! Server domain.
//!
//! `[server]` is a namespace container; actual settings live in named
//! subdomains (listen, api, web, auth, storage, artifacts, slatedb,
//! scheduler, logging, integrations). Same-host and split-host deployments
//! use the same schema.

use std::net::SocketAddr;
use std::time::Duration as StdDuration;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::duration::Duration;

/// A structurally resolved `[server]` view for consumers.
///
/// `Default` is intentionally not derived: any "default" `ServerNamespace`
/// would have empty `auth.methods`, which the resolver rejects. Construct
/// real values via `fabro_config::resolve_server` (production), or
/// `ServerNamespace::test_default()` behind the `test-support` feature
/// (tests).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerNamespace {
    pub listen:       ServerListenSettings,
    pub api:          ServerApiSettings,
    pub web:          ServerWebSettings,
    pub auth:         ServerAuthSettings,
    pub sandbox:      ServerSandboxSettings,
    pub storage:      ServerStorageSettings,
    pub artifacts:    ServerArtifactsSettings,
    pub slatedb:      ServerSlateDbSettings,
    pub scheduler:    ServerSchedulerSettings,
    pub logging:      ServerLoggingSettings,
    pub integrations: ServerIntegrationsSettings,
}

#[cfg(any(test, feature = "test-support"))]
impl ServerNamespace {
    /// A trivial `ServerNamespace` value suitable for serialization or
    /// destructuring tests. Auth methods are empty (would not pass
    /// `resolve_server`); use this only when the resolver is not in play.
    #[must_use]
    pub fn test_default() -> Self {
        Self {
            listen:       ServerListenSettings::default(),
            api:          ServerApiSettings::default(),
            web:          ServerWebSettings::default(),
            auth:         ServerAuthSettings::default(),
            sandbox:      ServerSandboxSettings::default(),
            storage:      ServerStorageSettings::default(),
            artifacts:    ServerArtifactsSettings::default(),
            slatedb:      ServerSlateDbSettings::default(),
            scheduler:    ServerSchedulerSettings::default(),
            logging:      ServerLoggingSettings::default(),
            integrations: ServerIntegrationsSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerListenSettings {
    Tcp {
        #[serde(
            serialize_with = "serialize_socket_addr",
            deserialize_with = "deserialize_socket_addr"
        )]
        address: SocketAddr,
    },
    Unix {
        path: String,
    },
}

impl Default for ServerListenSettings {
    fn default() -> Self {
        Self::Unix {
            path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerApiSettings {
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerWebSettings {
    pub enabled: bool,
    pub url:     String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAuthSettings {
    pub methods: Vec<ServerAuthMethod>,
    pub github:  ServerAuthGithubSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerAuthMethod {
    DevToken,
    Github,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAuthGithubSettings {
    pub allowed_usernames: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSandboxSettings {
    pub providers: ServerSandboxProvidersSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSandboxProvidersSettings {
    pub local:   ServerSandboxProviderSettings,
    pub docker:  ServerSandboxProviderSettings,
    pub daytona: ServerSandboxProviderSettings,
}

impl ServerSandboxProvidersSettings {
    /// Per-provider policy entry.
    #[must_use]
    pub fn for_provider(
        &self,
        provider: crate::SandboxProviderKind,
    ) -> &ServerSandboxProviderSettings {
        match provider {
            crate::SandboxProviderKind::Local => &self.local,
            crate::SandboxProviderKind::Docker => &self.docker,
            crate::SandboxProviderKind::Daytona => &self.daytona,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSandboxProviderSettings {
    pub enabled: bool,
}

impl Default for ServerSandboxProviderSettings {
    // The resolver defaults each provider to enabled; keep the struct default
    // aligned with that so callers that bypass the resolver behave identically.
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStorageSettings {
    pub root: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerArtifactsSettings {
    pub prefix: String,
    pub store:  ObjectStoreSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSlateDbSettings {
    pub prefix:         String,
    pub store:          ObjectStoreSettings,
    #[serde(
        serialize_with = "serialize_std_duration",
        deserialize_with = "deserialize_std_duration"
    )]
    pub flush_interval: StdDuration,
    pub disk_cache:     bool,
}

impl Default for ServerSlateDbSettings {
    fn default() -> Self {
        Self {
            prefix:         String::new(),
            store:          ObjectStoreSettings::default(),
            flush_interval: StdDuration::ZERO,
            disk_cache:     false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObjectStoreSettings {
    Local {
        root: String,
    },
    S3 {
        bucket:     String,
        region:     String,
        endpoint:   Option<String>,
        path_style: bool,
    },
}

impl Default for ObjectStoreSettings {
    fn default() -> Self {
        Self::Local {
            root: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSchedulerSettings {
    pub max_concurrent_runs: usize,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum LogDestination {
    #[default]
    File,
    Stdout,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLoggingSettings {
    pub level:       Option<String>,
    #[serde(default)]
    pub destination: LogDestination,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIntegrationsSettings {
    pub github: GithubIntegrationSettings,
    pub slack:  SlackIntegrationSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIntegrationSettings {
    pub enabled:   bool,
    pub strategy:  GithubIntegrationStrategy,
    pub app_id:    Option<String>,
    pub client_id: Option<String>,
    pub slug:      Option<String>,
    pub webhooks:  Option<IntegrationWebhooksSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackIntegrationSettings {
    pub enabled:         bool,
    pub default_channel: Option<String>,
}

impl Default for SlackIntegrationSettings {
    fn default() -> Self {
        Self {
            enabled:         true,
            default_channel: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationWebhooksSettings {
    pub strategy: Option<WebhookStrategy>,
}

fn serialize_socket_addr<S>(value: &SocketAddr, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

fn deserialize_socket_addr<'de, D>(deserializer: D) -> Result<SocketAddr, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    value.parse().map_err(D::Error::custom)
}

fn serialize_std_duration<S>(value: &StdDuration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&Duration::from_std(*value).to_string())
}

fn deserialize_std_duration<'de, D>(deserializer: D) -> Result<StdDuration, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Duration::deserialize(deserializer)?.as_std())
}

/// Closed enum of object-store providers. Unknown providers hard-fail
/// against the schema rather than passing through as opaque strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectStoreProvider {
    Local,
    S3,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubIntegrationStrategy {
    #[default]
    Token,
    App,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookStrategy {
    TailscaleFunnel,
    ServerUrl,
}
