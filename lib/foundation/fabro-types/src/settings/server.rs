//! Server domain.
//!
//! `[server]` is a namespace container; actual settings live in named
//! subdomains (listen, api, web, auth, storage, artifacts, scheduler,
//! logging, integrations). Same-host and split-host
//! deployments use the same schema.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::SandboxProviderKind;

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

/// `[server.sandbox]` — which sandbox providers this server may launch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSandboxSettings {
    pub providers: ServerSandboxProvidersSettings,
}

/// Per-provider policy keyed by provider kind.
///
/// The resolver always materializes the bundled kinds (`local`, `docker`,
/// `daytona`). Any other key names a sandbox-driver plugin executable and
/// carries its launch settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerSandboxProvidersSettings {
    pub entries: BTreeMap<SandboxProviderKind, ServerSandboxProviderSettings>,
}

impl ServerSandboxProvidersSettings {
    /// Policy for one provider kind, when the server knows the kind.
    #[must_use]
    pub fn get(&self, provider: &SandboxProviderKind) -> Option<&ServerSandboxProviderSettings> {
        self.entries.get(provider)
    }

    /// Whether the server may launch this provider. A kind without an entry
    /// is disabled: nothing configured it.
    #[must_use]
    pub fn is_enabled(&self, provider: &SandboxProviderKind) -> bool {
        self.get(provider).is_some_and(|entry| entry.enabled)
    }

    /// Kinds the server may launch, in key order.
    pub fn enabled_kinds(&self) -> impl Iterator<Item = &SandboxProviderKind> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.enabled)
            .map(|(kind, _)| kind)
    }

    /// Enabled kinds that are served by a plugin executable.
    pub fn enabled_plugins(
        &self,
    ) -> impl Iterator<Item = (&SandboxProviderKind, &SandboxPluginSettings)> {
        self.entries.iter().filter_map(|(kind, entry)| {
            (entry.enabled)
                .then_some(entry.plugin.as_ref())
                .flatten()
                .map(|plugin| (kind, plugin))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSandboxProviderSettings {
    pub enabled: bool,
    /// Launch settings for a plugin provider. Absent for bundled kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin:  Option<SandboxPluginSettings>,
}

impl Default for ServerSandboxProviderSettings {
    // The resolver defaults each bundled provider to enabled; keep the struct
    // default aligned with that so callers that bypass the resolver behave
    // identically.
    fn default() -> Self {
        Self {
            enabled: true,
            plugin:  None,
        }
    }
}

/// How the server launches a sandbox-driver plugin executable.
///
/// The executable speaks the sandbox-driver JSON-RPC protocol on stdio. It
/// starts with a scrubbed environment: only `env` and the ambient variables
/// named in `inherit_env` reach it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPluginSettings {
    /// Executable path. When absent the server searches `PATH` for
    /// `fabro-sandbox-<kind>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path:        Option<String>,
    /// Pinned SHA-256 of the executable, hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256:      Option<String>,
    /// Allow launching without a checksum.
    #[serde(default)]
    pub dev:         bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args:        Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env:         BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherit_env: Vec<String>,
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
