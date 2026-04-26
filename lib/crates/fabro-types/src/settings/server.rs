//! Server domain.
//!
//! `[server]` is a namespace container; actual settings live in named
//! subdomains (listen, api, web, auth, storage, artifacts, slatedb,
//! scheduler, logging, integrations). Same-host and split-host deployments
//! use the same schema.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration as StdDuration;

use ipnet::IpNet;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::duration::Duration;
use super::interp::InterpString;

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
    pub ip_allowlist: ServerIpAllowlistSettings,
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
            ip_allowlist: ServerIpAllowlistSettings::default(),
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
        path: InterpString,
    },
}

impl Default for ServerListenSettings {
    fn default() -> Self {
        Self::Unix {
            path: InterpString::parse(""),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerApiSettings {
    pub url: Option<InterpString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerWebSettings {
    pub enabled: bool,
    pub url:     InterpString,
}

impl Default for ServerWebSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url:     InterpString::parse(""),
        }
    }
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
pub struct ServerIpAllowlistSettings {
    pub entries:             Vec<IpAllowEntry>,
    pub trusted_proxy_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIpAllowlistOverrideSettings {
    pub entries:             Option<Vec<IpAllowEntry>>,
    pub trusted_proxy_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpAllowEntry {
    Literal(IpNet),
    GitHubMetaHooks,
}

impl IpAllowEntry {
    pub const GITHUB_META_HOOKS_KEYWORD: &str = "github_meta_hooks";

    pub fn parse_literal(value: &str) -> Result<Self, String> {
        value
            .parse::<IpNet>()
            .or_else(|_| value.parse::<std::net::IpAddr>().map(IpNet::from))
            .map_err(|error| error.to_string())
            .map(Self::Literal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStorageSettings {
    pub root: InterpString,
}

impl Default for ServerStorageSettings {
    fn default() -> Self {
        Self {
            root: InterpString::parse(""),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerArtifactsSettings {
    pub prefix: InterpString,
    pub store:  ObjectStoreSettings,
}

impl Default for ServerArtifactsSettings {
    fn default() -> Self {
        Self {
            prefix: InterpString::parse(""),
            store:  ObjectStoreSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSlateDbSettings {
    pub prefix:         InterpString,
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
            prefix:         InterpString::parse(""),
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
        root: InterpString,
    },
    S3 {
        bucket:     InterpString,
        region:     InterpString,
        endpoint:   Option<InterpString>,
        path_style: bool,
    },
}

impl Default for ObjectStoreSettings {
    fn default() -> Self {
        Self::Local {
            root: InterpString::parse(""),
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
    pub github:  GithubIntegrationSettings,
    pub slack:   SlackIntegrationSettings,
    pub discord: DiscordIntegrationSettings,
    pub teams:   TeamsIntegrationSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIntegrationSettings {
    pub enabled:     bool,
    pub strategy:    GithubIntegrationStrategy,
    pub app_id:      Option<InterpString>,
    pub client_id:   Option<InterpString>,
    pub slug:        Option<InterpString>,
    pub permissions: HashMap<String, InterpString>,
    pub webhooks:    Option<IntegrationWebhooksSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackIntegrationSettings {
    pub enabled:         bool,
    pub default_channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordIntegrationSettings {
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamsIntegrationSettings {
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationWebhooksSettings {
    pub strategy:     Option<WebhookStrategy>,
    pub ip_allowlist: Option<ServerIpAllowlistOverrideSettings>,
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
