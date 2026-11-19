//! The sandbox catalog Fabro gives Petri, and the provider connections the
//! server opens itself.
//!
//! Petri places every scope on a [`SandboxCatalog`]: Host, Docker and
//! Daytona are sandbox-driver built-ins that run in this process, so no
//! runtime Fabro builds launches a provider plugin, which a release build
//! cannot verify. Fabro starts from the catalog Petri derives from a run's
//! sandbox options (its environments, runner images, network policy and
//! Daytona resources) and replaces the Docker and Daytona entries and the
//! secret store with its own: the Docker endpoint this process's Docker
//! client uses, and the Daytona key from the vault. Nothing here reads a
//! secret from the process environment. Entries connect lazily, when a
//! scope is placed on them, so a Host-only run needs neither Docker nor
//! Daytona.
//!
//! Every Petri runtime Fabro builds starts from [`standard_runtime`] or
//! [`bare_runtime`].

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_static::EnvVars;
use petri_runtime::{Runtime, SandboxCatalog, SandboxOptions};
use sandbox_driver::legacy::SandboxProvider;
use sandbox_driver::{
    Error, ProviderEntry, ProviderKind, ProviderName, Sandboxes, SecretResolver, legacy,
};
use sandbox_driver_daytona::{DaytonaConfig, DaytonaProvider};
use sandbox_driver_docker::DockerProvider;

/// The secret name the built-in Daytona provider reads its API key from.
const DAYTONA_API_KEY: &str = "DAYTONA_API_KEY";

const USER_AGENT: &str = concat!("fabro-server/", env!("CARGO_PKG_VERSION"));

/// Explicit Daytona credentials: the SDK's configuration with the API key
/// always present and a `Debug` that never prints it. The process
/// environment is never consulted.
#[derive(Clone)]
pub struct DaytonaCredentials(DaytonaConfig);

impl DaytonaCredentials {
    /// Credentials for `api_key` against Daytona's public control plane,
    /// presenting Fabro's `User-Agent`.
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self(DaytonaConfig {
            api_key: Some(api_key),
            user_agent: Some(USER_AGENT.to_string()),
            ..DaytonaConfig::default()
        })
    }

    /// Credentials for a vault API key, with the control-plane URL and
    /// organization taken from `lookup` (server configuration). Nothing is
    /// read implicitly.
    ///
    /// These are the two settings the Daytona plugin read in the worker: no
    /// URL alias and no placement target, neither of which reached it.
    pub fn from_api_key(api_key: String, lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self::new(api_key)
            .with_api_url(lookup(EnvVars::DAYTONA_API_URL))
            .with_organization_id(lookup(EnvVars::DAYTONA_ORGANIZATION_ID))
    }

    /// The control-plane URL; Daytona's public API when `None`.
    #[must_use]
    pub fn with_api_url(mut self, api_url: Option<String>) -> Self {
        self.0.api_url = api_url;
        self
    }

    #[must_use]
    pub fn with_organization_id(mut self, organization_id: Option<String>) -> Self {
        self.0.organization_id = organization_id;
        self
    }

    /// A shared HTTP client; tests pass a no-proxy client here.
    #[must_use]
    pub fn with_http_client(mut self, http_client: Option<fabro_http::HttpClient>) -> Self {
        self.0.http_client = http_client;
        self
    }

    /// The SDK configuration the driver's Daytona provider connects with.
    #[must_use]
    fn config(&self) -> &DaytonaConfig {
        &self.0
    }
}

impl std::fmt::Debug for DaytonaCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaytonaCredentials")
            .field("api_url", &self.0.api_url)
            .field("organization_id", &self.0.organization_id)
            .finish_non_exhaustive()
    }
}

/// The provider configuration supplied to a run or prune. Defaults are
/// local Docker selection and no Daytona credentials; constructing this
/// configuration never connects to a backend or requires a credential.
#[derive(Clone, Debug, Default)]
pub struct SandboxProviderConfig {
    docker_host:         Option<String>,
    docker_host_address: Option<String>,
    daytona:             Option<DaytonaCredentials>,
}

impl SandboxProviderConfig {
    /// The configuration for `daytona`'s credentials, with how a remote
    /// Docker daemon's containers reach this machine from `lookup`.
    ///
    /// The Docker endpoint is read from this process's `DOCKER_HOST`, never
    /// from `lookup`: the server's Docker client connects to the daemon that
    /// variable names, so Petri's Docker entry names the daemon the server
    /// reaches the run's sandboxes on.
    pub fn from_lookup(
        daytona: Option<DaytonaCredentials>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Self {
        #[expect(
            clippy::disallowed_methods,
            reason = "the Docker client reads DOCKER_HOST from this process; Petri's Docker entry must name the same daemon"
        )]
        let docker_host = std::env::var(EnvVars::DOCKER_HOST).ok();
        Self {
            docker_host,
            docker_host_address: lookup(EnvVars::PETRI_SANDBOX_DOCKER_HOST_ADDRESS)
                .filter(|value| !value.trim().is_empty()),
            daytona,
        }
    }
}

/// Petri's standard runtime on Fabro's sandbox catalog for the default
/// sandbox options. A run installs its own options' catalog with
/// [`sandbox_catalog`].
#[must_use]
pub fn standard_runtime(config: &SandboxProviderConfig) -> Runtime {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a standard runtime is built, on Fabro's sandbox catalog"
    )]
    let runtime = Runtime::standard();
    runtime.sandbox_catalog(default_catalog(config))
}

/// Petri's bare runtime on Fabro's sandbox catalog for the default sandbox
/// options.
#[must_use]
pub fn bare_runtime(config: &SandboxProviderConfig) -> Runtime {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a bare runtime is built, on Fabro's sandbox catalog"
    )]
    let runtime = Runtime::bare();
    runtime.sandbox_catalog(default_catalog(config))
}

/// A live sandbox catalog over Fabro's entries, outside any run: the
/// server's own access to a run's sandbox through sandbox-driver's new API.
/// Entries connect lazily, when a sandbox is attached or created on them.
pub fn open_catalog(config: &SandboxProviderConfig) -> Result<Sandboxes, Error> {
    default_catalog(config).open()
}

fn default_catalog(config: &SandboxProviderConfig) -> SandboxCatalog {
    sandbox_catalog(config, &SandboxOptions::default())
        .expect("the default sandbox options describe a valid catalog")
}

/// The catalog a run's scopes are placed on: Petri's catalog for `options`
/// (an environment per backend, with the run's network policy, Daytona
/// resources and pinned runner images), with Fabro's Docker and Daytona
/// entries and Fabro's secrets in place of what Petri reads from the
/// process environment.
///
/// Fails as Petri does for options it cannot place, such as Daytona
/// resources below the runner's minimum.
pub fn sandbox_catalog(
    config: &SandboxProviderConfig,
    options: &SandboxOptions,
) -> Result<SandboxCatalog, String> {
    let mut docker = BTreeMap::new();
    if let Some(host) = config
        .docker_host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
    {
        docker.insert("host".to_owned(), host.into());
    }
    let mut daytona = BTreeMap::new();
    if let Some(credentials) = &config.daytona {
        let sdk = credentials.config();
        if let Some(api_url) = &sdk.api_url {
            daytona.insert("api_url".to_owned(), api_url.clone().into());
        }
        if let Some(organization) = &sdk.organization_id {
            daytona.insert("organization".to_owned(), organization.clone().into());
        }
    }
    // TODO(parity): the built-in Daytona provider presents sandbox-driver's
    // own `User-Agent` and its own HTTP client; Fabro's `fabro-server/<version>`
    // agent and an injected client (tests' no-proxy client) reach only the
    // server's own connections until the Daytona entry config carries them.
    let mut catalog = SandboxCatalog::from_options(options)?
        .with_provider(entry_name("docker"), entry("docker", docker, Vec::new()))
        .with_provider(
            entry_name("daytona"),
            entry("daytona", daytona, vec![DAYTONA_API_KEY.to_owned()]),
        )
        .with_secrets(Arc::new(VaultSecrets(config.daytona.clone())));
    if let Some(address) = &config.docker_host_address {
        catalog = catalog.with_host_address("docker", Some(address.clone()));
    }
    Ok(catalog)
}

fn entry_name(name: &str) -> ProviderName {
    ProviderName::try_new(name).expect("a built-in entry name is valid")
}

fn entry(
    kind: &str,
    config: BTreeMap<String, serde_json::Value>,
    secrets: Vec<String>,
) -> ProviderEntry {
    ProviderEntry {
        plugin: ProviderKind::try_new(kind).expect("a built-in provider kind is valid"),
        config,
        secrets,
        path: None,
        trust: None,
        args: Vec::new(),
        env: BTreeMap::new(),
        inherit_env: Some(Vec::new()),
    }
}

/// Entry secrets from the vault: the Daytona API key when Fabro has one,
/// and nothing else. A Daytona scope without a key fails when it is placed,
/// naming the missing `DAYTONA_API_KEY`; admission and a Host run never ask.
struct VaultSecrets(Option<DaytonaCredentials>);

#[async_trait]
impl SecretResolver for VaultSecrets {
    async fn resolve(&self, name: &str) -> Result<Option<String>, Error> {
        Ok(match name {
            DAYTONA_API_KEY => self
                .0
                .as_ref()
                .and_then(|credentials| credentials.config().api_key.clone()),
            _ => None,
        })
    }
}

/// The server's own connection to the run's Docker daemon, for the sandbox
/// page, Run Files, the terminal and Ask Fabro.
///
/// TODO(parity): this is sandbox-driver's legacy provider. The server keeps
/// it until the new API carries what those tabs use (git, a tty, listening
/// ports, and the legacy status fields): parity plan items B2 to B4, D2 and
/// A10. Petri's runs no longer go through it.
#[expect(
    clippy::result_large_err,
    reason = "preserve sandbox-driver's typed error"
)]
pub fn connect_docker() -> legacy::Result<Arc<dyn SandboxProvider>> {
    Ok(Arc::new(DockerProvider::connect_unverified()?))
}

/// The server's own Daytona connection, with only the vault credentials and
/// explicit control-plane configuration; no ambient secret fallback.
///
/// TODO(parity): sandbox-driver's legacy provider, kept for the server's
/// tabs until the new API carries git, a tty, SSH and VNC endpoints, and
/// the legacy status fields (parity plan B2 to B4, Y5, Y6, A10 and W).
pub async fn connect_daytona(
    credentials: &DaytonaCredentials,
) -> legacy::Result<Arc<dyn SandboxProvider>> {
    Ok(Arc::new(
        DaytonaProvider::connect_explicit(credentials.config().clone()).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_vault_resolves_only_the_daytona_key() {
        let vault = VaultSecrets(Some(DaytonaCredentials::new("test-key".to_string())));
        assert_eq!(
            vault.resolve(DAYTONA_API_KEY).await.unwrap().as_deref(),
            Some("test-key")
        );
        assert_eq!(vault.resolve("DAYTONA_JWT_TOKEN").await.unwrap(), None);
        assert_eq!(vault.resolve("HOME").await.unwrap(), None);
        assert_eq!(
            VaultSecrets(None).resolve(DAYTONA_API_KEY).await.unwrap(),
            None
        );
    }

    #[test]
    fn options_petri_cannot_place_are_refused() {
        let mut options = SandboxOptions::default();
        options.daytona_resources.cpu_cores = 1;
        assert!(sandbox_catalog(&SandboxProviderConfig::default(), &options).is_err());
        assert!(
            sandbox_catalog(
                &SandboxProviderConfig::default(),
                &SandboxOptions::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn daytona_configuration_ignores_the_url_alias_and_keeps_the_http_client() {
        let credentials = DaytonaCredentials::from_api_key("test-key".to_string(), |name| {
            (name == EnvVars::DAYTONA_SERVER_URL).then(|| "https://alias.example".to_string())
        })
        .with_http_client(Some(fabro_test::test_http_client()));
        assert_eq!(credentials.config().api_url, None);
        assert!(credentials.config().http_client.is_some());
    }

    #[test]
    fn provider_configuration_keeps_the_key_but_never_prints_it() {
        let key = "dtn_test_sensitive_value";
        let config = SandboxProviderConfig::from_lookup(
            Some(DaytonaCredentials::from_api_key(key.to_string(), |name| {
                (name == EnvVars::DAYTONA_ORGANIZATION_ID).then(|| "org-1".to_string())
            })),
            |_| None,
        );
        let daytona = config.daytona.as_ref().expect("the credentials are kept");
        assert_eq!(daytona.config().api_key.as_deref(), Some(key));
        let rendered = format!("{config:?}");
        assert!(!rendered.contains(key));
        assert!(rendered.contains("org-1"));
    }
}
