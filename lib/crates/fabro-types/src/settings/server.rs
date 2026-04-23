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

use super::duration::Duration as DurationLayer;
use super::interp::InterpString;
use super::maps::StickyMap;

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLoggingSettings {
    pub level: Option<String>,
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
    serializer.serialize_str(&DurationLayer::from_std(*value).to_string())
}

fn deserialize_std_duration<'de, D>(deserializer: D) -> Result<StdDuration, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(DurationLayer::deserialize(deserializer)?.as_std())
}

/// A sparse `[server]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen:       Option<ServerListenLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api:          Option<ServerApiLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web:          Option<ServerWebLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth:         Option<ServerAuthLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_allowlist: Option<ServerIpAllowlistLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage:      Option<ServerStorageLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts:    Option<ServerArtifactsLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slatedb:      Option<ServerSlateDbLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler:    Option<ServerSchedulerLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging:      Option<ServerLoggingLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrations: Option<ServerIntegrationsLayer>,
}

/// `[server.listen]` — shared bind transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "lowercase")]
pub enum ServerListenLayer {
    Tcp {
        #[serde(default)]
        address: Option<InterpString>,
    },
    Unix {
        #[serde(default)]
        path: Option<InterpString>,
    },
}

/// `[server.api]` — API surface settings.
///
/// `url` is an optional public URL; it is **not** derived from `server.listen`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerApiLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<InterpString>,
}

/// `[server.web]` — web surface settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerWebLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url:     Option<InterpString>,
}

/// `[server.auth]` — cohesive server auth surface.
///
/// When absent or resolved to no enabled API or web auth configuration, the
/// default server startup posture is fail-closed. Demo and test helpers may
/// explicitly opt in to insecure configurations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub methods: Option<Vec<ServerAuthMethod>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github:  Option<ServerAuthGithubLayer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthGithubLayer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_usernames: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerIpAllowlistLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries:             Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_proxy_count: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerIpAllowlistOverrideLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries:             Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_proxy_count: Option<u32>,
}

/// `[server.storage]` — single managed local disk root.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerStorageLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<InterpString>,
}

/// `[server.artifacts]` — object-store-backed artifact storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerArtifactsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ObjectStoreProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix:   Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local:    Option<ObjectStoreLocalLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3:       Option<ObjectStoreS3Layer>,
}

/// `[server.slatedb]` — SlateDB bottomless storage plus tunables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerSlateDbLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:       Option<ObjectStoreProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix:         Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flush_interval: Option<DurationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local:          Option<ObjectStoreLocalLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3:             Option<ObjectStoreS3Layer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_cache:     Option<bool>,
}

/// Closed enum of object-store providers. Unknown providers hard-fail
/// against the schema rather than passing through as opaque strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectStoreProvider {
    Local,
    S3,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectStoreLocalLayer {
    /// Overrides the default root, which otherwise falls back to
    /// `{server.storage.root}/objects/{domain}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectStoreS3Layer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket:     Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region:     Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint:   Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_style: Option<bool>,
}

/// `[server.scheduler]` — server-managed execution policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerSchedulerLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_runs: Option<usize>,
}

/// `[server.logging]` — process-owned logging configuration for the server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerLoggingLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// `[server.integrations.<provider>]` — cohesive integration surface for chat
/// platforms and git providers (GitHub App, webhooks, etc.). First-pass
/// integrations enumerate known providers rather than using a flatten-HashMap
/// shape so strict unknown-field validation still holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ServerIntegrationsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github:  Option<GithubIntegrationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack:   Option<SlackIntegrationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord: Option<DiscordIntegrationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teams:   Option<TeamsIntegrationLayer>,
}

/// `[server.integrations.github]` — GitHub App, credentials, and inbound
/// webhooks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct GithubIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled:     Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy:    Option<GithubIntegrationStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id:      Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id:   Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug:        Option<InterpString>,
    #[serde(default, skip_serializing_if = "StickyMap::is_empty")]
    pub permissions: StickyMap<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhooks:    Option<IntegrationWebhooksLayer>,
}

/// `[server.integrations.slack]` — Slack workspace credentials and defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct SlackIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled:         Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_channel: Option<InterpString>,
}

/// `[server.integrations.discord]` — Discord workspace configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct DiscordIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// `[server.integrations.teams]` — Microsoft Teams configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct TeamsIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct IntegrationWebhooksLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy:     Option<WebhookStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_allowlist: Option<ServerIpAllowlistOverrideLayer>,
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
