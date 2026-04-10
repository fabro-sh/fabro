//! Server domain.
//!
//! `[server]` is a namespace container; actual settings live in named
//! subdomains (listen, api, web, auth, storage, artifacts, slatedb,
//! scheduler, logging, integrations). Same-host and split-host deployments
//! use the same schema.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};

use super::duration::Duration as DurationLayer;
use super::interp::InterpString;

/// A structurally resolved `[server]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSettings {
    pub listen: ServerListenSettings,
    pub api: ServerApiSettings,
    pub web: ServerWebSettings,
    pub auth: ServerAuthSettings,
    pub storage: ServerStorageSettings,
    pub artifacts: ServerArtifactsSettings,
    pub slatedb: ServerSlateDbSettings,
    pub scheduler: ServerSchedulerSettings,
    pub logging: ServerLoggingSettings,
    pub integrations: ServerIntegrationsSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerListenSettings {
    Tcp {
        address: SocketAddr,
        tls: Option<TlsConfig>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsConfig {
    pub cert: InterpString,
    pub key: InterpString,
    pub ca: InterpString,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            cert: InterpString::parse(""),
            key: InterpString::parse(""),
            ca: InterpString::parse(""),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerApiSettings {
    pub url: Option<InterpString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerWebSettings {
    pub enabled: bool,
    pub url: InterpString,
}

impl Default for ServerWebSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: InterpString::parse(""),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerAuthSettings {
    pub api: ServerAuthApiSettings,
    pub web: ServerAuthWebSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerAuthApiSettings {
    pub jwt: Option<ServerAuthApiJwtSettings>,
    pub mtls: Option<ServerAuthApiMtlsSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerAuthApiJwtSettings {
    pub enabled: bool,
    pub issuer: Option<InterpString>,
    pub audience: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerAuthApiMtlsSettings {
    pub enabled: bool,
    pub ca: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerAuthWebSettings {
    pub allowed_usernames: Vec<String>,
    pub providers: ServerAuthWebProvidersSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerAuthWebProvidersSettings {
    pub github: Option<GithubOauthSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GithubOauthSettings {
    pub enabled: bool,
    pub client_id: Option<InterpString>,
    pub client_secret: Option<InterpString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerArtifactsSettings {
    pub prefix: InterpString,
    pub store: ObjectStoreSettings,
}

impl Default for ServerArtifactsSettings {
    fn default() -> Self {
        Self {
            prefix: InterpString::parse(""),
            store: ObjectStoreSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSlateDbSettings {
    pub prefix: InterpString,
    pub store: ObjectStoreSettings,
    pub flush_interval: StdDuration,
}

impl Default for ServerSlateDbSettings {
    fn default() -> Self {
        Self {
            prefix: InterpString::parse(""),
            store: ObjectStoreSettings::default(),
            flush_interval: StdDuration::ZERO,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectStoreSettings {
    Local {
        root: InterpString,
    },
    S3 {
        bucket: InterpString,
        region: InterpString,
        endpoint: Option<InterpString>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSchedulerSettings {
    pub max_concurrent_runs: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerLoggingSettings {
    pub level: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerIntegrationsSettings {
    pub github: GithubIntegrationSettings,
    pub slack: SlackIntegrationSettings,
    pub discord: DiscordIntegrationSettings,
    pub teams: TeamsIntegrationSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GithubIntegrationSettings {
    pub enabled: bool,
    pub app_id: Option<InterpString>,
    pub client_id: Option<InterpString>,
    pub slug: Option<InterpString>,
    pub permissions: HashMap<String, InterpString>,
    pub webhooks: Option<IntegrationWebhooksSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackIntegrationSettings {
    pub enabled: bool,
    pub default_channel: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordIntegrationSettings {
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamsIntegrationSettings {
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrationWebhooksSettings {
    pub strategy: Option<WebhookStrategy>,
}

/// A sparse `[server]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<ServerListenLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ServerApiLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<ServerWebLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<ServerAuthLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<ServerStorageLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<ServerArtifactsLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slatedb: Option<ServerSlateDbLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<ServerSchedulerLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<ServerLoggingLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrations: Option<ServerIntegrationsLayer>,
}

/// `[server.listen]` — shared bind transport. TLS lives under `[server.listen.tls]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "lowercase")]
pub enum ServerListenLayer {
    Tcp {
        #[serde(default)]
        address: Option<InterpString>,
        #[serde(default)]
        tls: Option<ServerListenTlsLayer>,
    },
    Unix {
        #[serde(default)]
        path: Option<InterpString>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerListenTlsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<InterpString>,
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerWebLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<InterpString>,
}

/// `[server.auth]` — cohesive server auth surface.
///
/// When absent or resolved to no enabled API or web auth configuration, the
/// default server startup posture is fail-closed. Demo and test helpers may
/// explicitly opt in to insecure configurations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ServerAuthApiLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<ServerAuthWebLayer>,
}

/// `[server.auth.api]` — supports multiple strategies concurrently. Each
/// strategy is a named subtable: `[server.auth.api.jwt]`, `[server.auth.api.mtls]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthApiLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwt: Option<ServerAuthApiJwtLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtls: Option<ServerAuthApiMtlsLayer>,
}

/// `[server.auth.api.jwt]` — JWT auth strategy fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthApiJwtLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<InterpString>,
}

/// `[server.auth.api.mtls]` — mutual TLS auth strategy fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthApiMtlsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<InterpString>,
}

/// `[server.auth.web]` — provider-neutral access rules plus keyed providers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthWebLayer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_usernames: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<ServerAuthWebProvidersLayer>,
}

/// `[server.auth.web.providers.<provider>]` — web auth providers keyed by
/// provider name. First-pass providers cover GitHub.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthWebProvidersLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<ServerAuthWebGithubLayer>,
}

/// `[server.auth.web.providers.github]` — GitHub OAuth configuration fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerAuthWebGithubLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<InterpString>,
}

/// `[server.storage]` — single managed local disk root.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerStorageLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<InterpString>,
}

/// `[server.artifacts]` — object-store-backed artifact storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerArtifactsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ObjectStoreProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<ObjectStoreLocalLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<ObjectStoreS3Layer>,
}

/// `[server.slatedb]` — SlateDB bottomless storage plus tunables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSlateDbLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ObjectStoreProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flush_interval: Option<DurationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<ObjectStoreLocalLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<ObjectStoreS3Layer>,
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
    /// `server.storage.root`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectStoreS3Layer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_style: Option<bool>,
}

/// `[server.scheduler]` — server-managed execution policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerIntegrationsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubIntegrationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackIntegrationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord: Option<DiscordIntegrationLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teams: Option<TeamsIntegrationLayer>,
}

/// `[server.integrations.github]` — GitHub App, credentials, and inbound webhooks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<InterpString>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub permissions: HashMap<String, InterpString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhooks: Option<IntegrationWebhooksLayer>,
}

/// `[server.integrations.slack]` — Slack workspace credentials and defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_channel: Option<InterpString>,
}

/// `[server.integrations.discord]` — Discord workspace configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// `[server.integrations.teams]` — Microsoft Teams configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamsIntegrationLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationWebhooksLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<WebhookStrategy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookStrategy {
    TailscaleFunnel,
}
