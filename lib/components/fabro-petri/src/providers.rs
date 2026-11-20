//! The sandbox catalog Fabro gives Petri, and the catalog the server opens
//! itself to reach a run's sandboxes.
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
use sandbox_driver::{
    Environment, EnvironmentName, Error, ProviderEntry, ProviderKind, ProviderName, SandboxSpec,
    Sandboxes, SecretResolver, Source, StaticEnvironments,
};
use sandbox_driver_protocol::PluginLoader;

/// The secret name the built-in Daytona provider reads its API key from.
const DAYTONA_API_KEY: &str = "DAYTONA_API_KEY";

/// Explicit Daytona credentials: the API key, always present, with the
/// control plane's URL and organization, and a `Debug` that never prints the
/// key. The process environment is never consulted.
#[derive(Clone)]
pub struct DaytonaCredentials {
    api_key:         String,
    api_url:         Option<String>,
    organization_id: Option<String>,
}

impl DaytonaCredentials {
    /// Credentials for `api_key` against Daytona's public control plane.
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: None,
            organization_id: None,
        }
    }

    /// Credentials for a vault API key, with the control-plane URL and
    /// organization taken from `lookup` (server configuration). Nothing is
    /// read implicitly. There is no URL alias and no placement target.
    pub fn from_api_key(api_key: String, lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self::new(api_key)
            .with_api_url(lookup(EnvVars::DAYTONA_API_URL))
            .with_organization_id(lookup(EnvVars::DAYTONA_ORGANIZATION_ID))
    }

    /// The control-plane URL; Daytona's public API when `None`.
    #[must_use]
    pub fn with_api_url(mut self, api_url: Option<String>) -> Self {
        self.api_url = api_url;
        self
    }

    #[must_use]
    pub fn with_organization_id(mut self, organization_id: Option<String>) -> Self {
        self.organization_id = organization_id;
        self
    }
}

impl std::fmt::Debug for DaytonaCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaytonaCredentials")
            .field("api_url", &self.api_url)
            .field("organization_id", &self.organization_id)
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

    /// The Daytona credentials, when the vault holds a key.
    #[must_use]
    pub fn daytona(&self) -> Option<&DaytonaCredentials> {
        self.daytona.as_ref()
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

/// The server's own catalog, outside any run: Fabro's Host, Docker and
/// Daytona entries, a `host` environment for designating a run's directory,
/// and `plugins`, the operator's third-party provider entries, which
/// sandbox-driver launches over protocol 3. Entries connect lazily, when a
/// sandbox is attached or created on them, or their health is asked.
pub fn open_catalog(
    config: &SandboxProviderConfig,
    plugins: impl IntoIterator<Item = (ProviderName, ProviderEntry)>,
) -> Result<Sandboxes, Error> {
    let host = entry_name("host");
    let environments = StaticEnvironments(BTreeMap::from([(
        EnvironmentName::try_new("host").expect("a built-in environment name is valid"),
        Environment {
            provider: host.clone(),
            spec:     SandboxSpec::new(Source::HostDirectory),
        },
    )]));
    let providers = [
        (host, entry("host", BTreeMap::new(), Vec::new())),
        (entry_name("docker"), docker_entry(config)),
        (entry_name("daytona"), daytona_entry(config)),
    ]
    .into_iter()
    .chain(plugins);
    let registry = SandboxCatalog::builtin_registry().plugins(Arc::new(PluginLoader::default()));
    SandboxCatalog::new(
        providers,
        Arc::new(environments),
        registry,
        Arc::new(VaultSecrets(config.daytona.clone())),
    )
    .open()
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
    // TODO(parity): sandbox-driver's Daytona entry config has no field for
    // the client's `User-Agent` or for an HTTP client the application
    // supplies, so the built-in provider sends `sandbox-driver/<version>`
    // and uses its own client: Fabro's `fabro-server/<version>` agent and
    // its proxy policy do not reach Daytona.
    let mut catalog = SandboxCatalog::from_options(options)?
        .with_provider(entry_name("docker"), docker_entry(config))
        .with_provider(entry_name("daytona"), daytona_entry(config))
        .with_secrets(Arc::new(VaultSecrets(config.daytona.clone())));
    if let Some(address) = &config.docker_host_address {
        catalog = catalog.with_host_address("docker", Some(address.clone()));
    }
    Ok(catalog)
}

/// The Docker entry: the daemon this process's `DOCKER_HOST` names.
fn docker_entry(config: &SandboxProviderConfig) -> ProviderEntry {
    let mut docker = BTreeMap::new();
    if let Some(host) = config
        .docker_host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
    {
        docker.insert("host".to_owned(), host.into());
    }
    entry("docker", docker, Vec::new())
}

/// The Daytona entry: the configured control plane, with the API key
/// resolved from the vault.
fn daytona_entry(config: &SandboxProviderConfig) -> ProviderEntry {
    let mut daytona = BTreeMap::new();
    if let Some(credentials) = &config.daytona {
        if let Some(api_url) = &credentials.api_url {
            daytona.insert("api_url".to_owned(), api_url.clone().into());
        }
        if let Some(organization) = &credentials.organization_id {
            daytona.insert("organization".to_owned(), organization.clone().into());
        }
    }
    entry("daytona", daytona, vec![DAYTONA_API_KEY.to_owned()])
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
                .map(|credentials| credentials.api_key.clone()),
            _ => None,
        })
    }
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
    fn daytona_configuration_ignores_the_url_alias() {
        let credentials = DaytonaCredentials::from_api_key("test-key".to_string(), |name| {
            (name == EnvVars::DAYTONA_SERVER_URL).then(|| "https://alias.example".to_string())
        });
        assert_eq!(credentials.api_url, None);
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
        assert_eq!(daytona.api_key, key);
        let rendered = format!("{config:?}");
        assert!(!rendered.contains(key));
        assert!(rendered.contains("org-1"));
    }
}
