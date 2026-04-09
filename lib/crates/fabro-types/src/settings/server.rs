//! Server domain.
//!
//! `[server]` is a namespace container; actual settings live in named
//! subdomains (listen, api, web, auth, storage, artifacts, slatedb,
//! scheduler, logging, integrations). Same-host and split-host deployments
//! use the same schema.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::duration::Duration;
use super::interp::InterpString;

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
    pub flush_interval: Option<Duration>,
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
