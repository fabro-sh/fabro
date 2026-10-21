//! Built-in sandbox providers shared by Petri execution and server access.
//!
//! Configuration is captured by the caller, with Daytona credentials from
//! the vault. Factories connect lazily per run; a Host-only run needs neither
//! Docker nor Daytona. Host registry ownership stays with Petri: server
//! attach uses an observer instead of these factories.
//!
//! Every Petri runtime Fabro builds starts from [`standard_runtime`] or
//! [`bare_runtime`], so none falls back to launching a provider plugin,
//! which a release build cannot verify.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use fabro_static::EnvVars;
use fabro_types::SandboxProviderKind;
use fabro_types::settings::server::{SandboxPluginSettings, ServerSandboxProvidersSettings};
use petri_runtime::{
    InProcessProviders, ProviderContext, ProviderFactory, ProviderNetwork, ProviderRegistrations,
    Runtime, fingerprint,
};
use sandbox_driver::{AuthError, Error, ProviderKind, SandboxProvider, SandboxSpec};
use sandbox_driver_daytona::{DaytonaConfig, DaytonaProvider};
use sandbox_driver_docker::DockerProvider;
use sandbox_driver_host::HostProvider;
use sandbox_driver_protocol::PluginConfig;

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
    /// These are the two settings the Daytona plugin read in the worker, so
    /// a lease it recorded keeps its fingerprint: no URL alias and no
    /// placement target, neither of which reached the plugin.
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
    /// The server's enabled third-party providers, each as Petri registers
    /// it, or why its settings cannot be registered.
    plugins:             BTreeMap<SandboxProviderKind, Result<RegisteredPlugin, String>>,
}

/// A third-party provider as Petri registers it: how its plugin launches,
/// and the template its process scopes are built from.
#[derive(Clone, Debug)]
struct RegisteredPlugin {
    config:       PluginConfig,
    process_spec: SandboxSpec,
}

impl SandboxProviderConfig {
    /// The configuration for `daytona`'s credentials, with how a remote
    /// Docker daemon's containers reach this machine from `lookup`.
    ///
    /// The Docker endpoint is read from this process's `DOCKER_HOST`, never
    /// from `lookup`: the Docker client connects to the daemon that
    /// variable names, so the lease fingerprint and network name the daemon
    /// the sandboxes are actually created on.
    pub fn from_lookup(
        daytona: Option<DaytonaCredentials>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Self {
        #[expect(
            clippy::disallowed_methods,
            reason = "the Docker client reads DOCKER_HOST from this process; the fingerprint must name the same daemon"
        )]
        let docker_host = std::env::var(EnvVars::DOCKER_HOST).ok();
        Self {
            docker_host,
            docker_host_address: lookup(EnvVars::PETRI_SANDBOX_DOCKER_HOST_ADDRESS)
                .filter(|value| !value.trim().is_empty()),
            daytona,
            plugins: BTreeMap::new(),
        }
    }

    /// This configuration with the server's enabled third-party providers
    /// from `server.sandbox.providers`. Petri launches a registered plugin
    /// with exactly the environment it is given, so each name in a
    /// provider's `inherit_env` is resolved here, from `lookup`.
    #[must_use]
    pub fn with_plugins(
        mut self,
        providers: &ServerSandboxProvidersSettings,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Self {
        self.plugins = providers
            .enabled_plugins()
            .map(|(kind, settings)| (kind.clone(), registered_plugin(kind, settings, &lookup)))
            .collect();
        self
    }

    /// Why `kind`'s provider cannot be registered for a run; `None` for a
    /// bundled kind and for a provider that registers.
    pub(crate) fn registration_problem(&self, kind: &SandboxProviderKind) -> Option<String> {
        if kind.bundled().is_some() {
            return None;
        }
        match self.plugins.get(kind) {
            None => Some(format!(
                "sandbox provider `{kind}` is not an enabled provider in server.sandbox.providers"
            )),
            Some(Err(problem)) => Some(problem.clone()),
            Some(Ok(_)) => None,
        }
    }

    /// The built-in providers, linked in process, and every third-party
    /// provider that registers.
    fn registrations(&self) -> ProviderRegistrations {
        let mut registrations = ProviderRegistrations::linked(built_in_providers(self));
        for (kind, plugin) in &self.plugins {
            let Ok(plugin) = plugin else { continue };
            if let Err(error) =
                registrations.register_plugin(plugin.config.clone(), plugin.process_spec.clone())
            {
                tracing::warn!(provider = %kind, %error, "sandbox provider not registered");
            }
        }
        registrations
    }
}

/// `kind`'s settings as Petri registers them, checked as Petri checks them.
fn registered_plugin(
    kind: &SandboxProviderKind,
    settings: &SandboxPluginSettings,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<RegisteredPlugin, String> {
    let invalid = |reason: String| format!("sandbox provider `{kind}`: {reason}");
    let provider_kind =
        ProviderKind::try_new(kind.as_str()).map_err(|error| invalid(error.to_string()))?;
    let template = settings
        .process_spec
        .clone()
        .ok_or_else(|| invalid("`process_spec` is required".to_string()))?;
    let process_spec: SandboxSpec = serde_json::from_value(template).map_err(|error| {
        invalid(format!(
            "`process_spec` is not a sandbox specification: {error}"
        ))
    })?;
    let mut env = settings.env.clone();
    for name in &settings.inherit_env {
        if let Some(value) = lookup(name) {
            env.entry(name.clone()).or_insert(value);
        }
    }
    let mut config = PluginConfig::new(provider_kind);
    config.path = settings.path.as_deref().map(PathBuf::from);
    config.sha256.clone_from(&settings.sha256);
    config.dev = settings.dev;
    config.args.clone_from(&settings.args);
    config.env = env;
    let plugin = RegisteredPlugin {
        config,
        process_spec,
    };
    ProviderRegistrations::new()
        .register_plugin(plugin.config.clone(), plugin.process_spec.clone())
        .map_err(|error| invalid(error.to_string()))?;
    Ok(plugin)
}

/// Petri's standard runtime with Fabro's built-in providers installed.
#[must_use]
pub fn standard_runtime(config: &SandboxProviderConfig) -> Runtime {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a standard runtime is built, with the built-in providers installed"
    )]
    let runtime = Runtime::standard();
    runtime.sandbox_providers(config.registrations())
}

/// Petri's bare runtime with Fabro's built-in providers installed.
#[must_use]
pub fn bare_runtime(config: &SandboxProviderConfig) -> Runtime {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a bare runtime is built, with the built-in providers installed"
    )]
    let runtime = Runtime::bare();
    runtime.sandbox_providers(config.registrations())
}

/// The Docker connection used by both the server and Petri. The driver reads
/// the caller process's Docker endpoint/TLS environment; health is checked
/// by the caller so diagnostics can report an unavailable daemon.
#[expect(
    clippy::result_large_err,
    reason = "preserve sandbox-driver's typed error, as required by Petri's ProviderFactory contract"
)]
pub fn connect_docker() -> sandbox_driver::Result<Arc<dyn SandboxProvider>> {
    Ok(Arc::new(DockerProvider::connect_unverified()?))
}

/// Connect using only the vault credentials and explicit control-plane
/// configuration; no ambient secret fallback is permitted.
pub async fn connect_daytona(
    credentials: &DaytonaCredentials,
) -> sandbox_driver::Result<Arc<dyn SandboxProvider>> {
    Ok(Arc::new(
        DaytonaProvider::connect_explicit(credentials.config().clone()).await?,
    ))
}

/// One lazy factory per built-in kind. Missing Daytona credentials fail
/// only when a Daytona scope is acquired, never for admission or a Host run.
fn built_in_providers(config: &SandboxProviderConfig) -> InProcessProviders {
    InProcessProviders::new()
        .with(Arc::new(HostFactory))
        .with(Arc::new(DockerFactory {
            host:         config.docker_host.clone(),
            host_address: config.docker_host_address.clone(),
        }))
        .with(Arc::new(DaytonaFactory(config.daytona.clone())))
}

struct HostFactory;

#[async_trait]
impl ProviderFactory for HostFactory {
    fn kind(&self) -> &'static str {
        "host"
    }

    /// An empty registry path when Petri supplies none, as the plugin
    /// recorded it.
    fn fingerprint_seed(&self, context: &ProviderContext) -> String {
        fingerprint::host(context.host_registry().unwrap_or(Path::new("")))
    }

    fn network(&self) -> ProviderNetwork {
        ProviderNetwork::host()
    }

    async fn connect(
        &self,
        context: &ProviderContext,
    ) -> sandbox_driver::Result<Arc<dyn SandboxProvider>> {
        let registry = context.host_registry().ok_or_else(|| {
            Error::invalid_spec(
                "host_registry",
                "Petri supplied no Host registry for this run",
            )
        })?;
        Ok(Arc::new(HostProvider::with_registry(registry).await?))
    }
}

struct DockerFactory {
    host:         Option<String>,
    host_address: Option<String>,
}

impl DockerFactory {
    fn seed(&self) -> String {
        fingerprint::docker(self.host.as_deref())
    }
}

#[async_trait]
impl ProviderFactory for DockerFactory {
    fn kind(&self) -> &'static str {
        "docker"
    }

    fn fingerprint_seed(&self, _context: &ProviderContext) -> String {
        self.seed()
    }

    fn network(&self) -> ProviderNetwork {
        ProviderNetwork::docker(self.host.as_deref(), self.host_address.as_deref())
    }

    async fn connect(
        &self,
        _context: &ProviderContext,
    ) -> sandbox_driver::Result<Arc<dyn SandboxProvider>> {
        connect_docker()
    }
}

struct DaytonaFactory(Option<DaytonaCredentials>);

impl DaytonaFactory {
    fn seed(&self) -> String {
        let config = self.0.as_ref().map(DaytonaCredentials::config);
        fingerprint::daytona(
            config.and_then(|config| config.api_url.as_deref()),
            config.and_then(|config| config.organization_id.as_deref()),
            None,
        )
    }
}

#[async_trait]
impl ProviderFactory for DaytonaFactory {
    fn kind(&self) -> &'static str {
        "daytona"
    }

    fn fingerprint_seed(&self, _context: &ProviderContext) -> String {
        self.seed()
    }

    fn network(&self) -> ProviderNetwork {
        ProviderNetwork::none()
    }

    async fn connect(
        &self,
        _context: &ProviderContext,
    ) -> sandbox_driver::Result<Arc<dyn SandboxProvider>> {
        let credentials = self.0.as_ref().ok_or_else(|| Error::Auth(AuthError::new(
            ProviderKind::try_new("daytona").expect("the built-in provider kind is valid"),
            "Daytona requires DAYTONA_API_KEY in the vault; run `fabro secret set DAYTONA_API_KEY`",
        )))?;
        connect_daytona(credentials).await
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::server::ServerSandboxProviderSettings;

    use super::*;

    fn kubernetes() -> SandboxProviderKind {
        SandboxProviderKind::try_new("kubernetes").expect("a valid kind")
    }

    fn providers(settings: SandboxPluginSettings) -> ServerSandboxProvidersSettings {
        let mut providers = ServerSandboxProvidersSettings::default();
        providers
            .entries
            .insert(kubernetes(), ServerSandboxProviderSettings {
                enabled: true,
                plugin:  Some(settings),
            });
        providers
    }

    fn plugin_settings() -> SandboxPluginSettings {
        SandboxPluginSettings {
            path: Some("/usr/local/bin/sandbox-driver-kubernetes".to_string()),
            dev: true,
            args: vec!["--config".to_string(), "/etc/k8s.toml".to_string()],
            env: BTreeMap::from([("KUBE_NAMESPACE".to_string(), "factory".to_string())]),
            inherit_env: vec!["HOME".to_string(), "UNSET".to_string()],
            process_spec: Some(serde_json::json!({
                "source": {"image": {"reference": "ghcr.io/acme/runner:1"}},
                "resources": {"cpu_cores": 1, "memory_mb": 1024},
            })),
            ..SandboxPluginSettings::default()
        }
    }

    #[test]
    fn a_configured_plugin_registers_with_its_inherited_variables_resolved() {
        let config = SandboxProviderConfig::default()
            .with_plugins(&providers(plugin_settings()), |name| {
                (name == "HOME").then(|| "/home/fabro".to_string())
            });
        assert_eq!(config.registration_problem(&kubernetes()), None);
        let Some(Ok(plugin)) = config.plugins.get(&kubernetes()) else {
            panic!("the plugin registers");
        };
        // Petri launches a registered plugin with exactly this environment.
        assert!(plugin.config.inherit_env.is_empty());
        assert_eq!(
            plugin.config.env.get("HOME").map(String::as_str),
            Some("/home/fabro")
        );
        assert_eq!(
            plugin.config.env.get("KUBE_NAMESPACE").map(String::as_str),
            Some("factory")
        );
        assert!(!plugin.config.env.contains_key("UNSET"));
        assert_eq!(plugin.config.args, vec!["--config", "/etc/k8s.toml"]);
        assert_eq!(plugin.process_spec.resources.cpu_cores, Some(1));
    }

    #[test]
    fn a_plugin_without_a_usable_template_or_path_is_reported_not_registered() {
        let missing = SandboxPluginSettings {
            process_spec: None,
            ..plugin_settings()
        };
        let relative = SandboxPluginSettings {
            path: Some("sandbox-driver-kubernetes".to_string()),
            ..plugin_settings()
        };
        for (settings, expected) in [(missing, "process_spec"), (relative, "absolute")] {
            let config =
                SandboxProviderConfig::default().with_plugins(&providers(settings), |_| None);
            let problem = config
                .registration_problem(&kubernetes())
                .expect("the provider does not register");
            assert!(problem.contains(expected), "{problem}");
        }
        let unconfigured = SandboxProviderConfig::default()
            .registration_problem(&kubernetes())
            .expect("an unconfigured kind is reported");
        assert!(
            unconfigured.contains("server.sandbox.providers"),
            "{unconfigured}"
        );
        assert_eq!(
            SandboxProviderConfig::default().registration_problem(&SandboxProviderKind::DOCKER),
            None
        );
    }

    #[test]
    fn docker_fingerprint_keeps_the_plugin_namespace_for_unset_and_configured_hosts() {
        for (host, expected) in [
            (None, "docker:default"),
            (Some("  "), "docker:default"),
            (Some(" tcp://daemon:2376 "), "docker:tcp://daemon:2376"),
            (
                Some("unix:///var/run/docker.sock"),
                "docker:unix:///var/run/docker.sock",
            ),
        ] {
            let factory = DockerFactory {
                host:         host.map(str::to_owned),
                host_address: None,
            };
            assert_eq!(factory.seed(), expected);
        }
    }

    #[test]
    fn daytona_fingerprint_keeps_the_plugin_seed() {
        let unset = DaytonaCredentials::from_api_key("test-key".to_string(), |_| None);
        assert_eq!(DaytonaFactory(Some(unset)).seed(), "daytona:::");
        let configured =
            DaytonaCredentials::from_api_key("test-key".to_string(), |name| match name {
                EnvVars::DAYTONA_API_URL => Some("https://daytona.example".to_string()),
                EnvVars::DAYTONA_SERVER_URL => Some("https://ignored.example".to_string()),
                EnvVars::DAYTONA_ORGANIZATION_ID => Some("org-1".to_string()),
                EnvVars::DAYTONA_TARGET => Some("us".to_string()),
                _ => None,
            });
        let factory = DaytonaFactory(Some(configured));
        assert_eq!(factory.seed(), "daytona:https://daytona.example:org-1:");
        assert_eq!(factory.region(), None);
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
