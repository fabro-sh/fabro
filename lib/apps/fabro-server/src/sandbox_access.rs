//! The server's direct access to run sandboxes through the sandbox driver.
//!
//! Petri creates every run sandbox and records it: the provider, the
//! provider's id and the working directory travel on `scope.acquired` into
//! the run's [`RunSandboxInstance`], and every Docker or Daytona sandbox
//! carries the `petri.run` label with the run id. The server reaches a
//! run's sandbox for the sandbox tab, Run Files, the terminal, SSH, preview
//! URLs, VNC, `fabro cp` and Ask Fabro by connecting the record's provider
//! itself and attaching to the record's id, without going through Petri.
//!
//! Ownership is keyed on `petri.run`: a persisted id is acted on only when
//! the sandbox behind it still carries the run's label, so an id that has
//! come to name someone else's sandbox on a shared daemon is refused. A host
//! sandbox is a managed directory the run's worker recorded, with the same
//! labels, in the host registry inside the run's Petri directory; the
//! server reads that registry without writing it and attaches to the
//! directory by path, so the run's ownership is checked on the host as it
//! is on Docker. Only a directory no registry records is designated again
//! without a record.
//!
//! Credentials arrive explicitly. Nothing here reads the process
//! environment for a secret: the Daytona key comes from the vault through
//! [`DaytonaCredentials`]. The Docker client resolves its endpoint from the
//! same variables Fabro forwards to its worker (`DOCKER_HOST` and
//! its TLS companions), so the server and the run's containers meet on one
//! daemon.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use fabro_config::Storage;
use fabro_petri::providers;
pub(crate) use fabro_petri::providers::DaytonaCredentials;
use fabro_types::settings::run::RunLifecyclePolicy;
use fabro_types::settings::server::{
    SandboxPluginSettings, ServerSandboxProviderSettings, ServerSandboxProvidersSettings,
};
use fabro_types::{
    BundledProvider, RunId, RunSandboxInstance, SandboxInfo, SandboxListMeta, SandboxListResponse,
    SandboxProviderKind, SandboxProviderLookupError,
};
use futures_util::future::join_all;
use sandbox_driver::{
    Error as DriverError, HealthStatus, OwnedProvider, Ownership, ProviderHealth, ProviderKind,
    Sandbox, SandboxFilter, SandboxId, SandboxProvider, SandboxSource, SandboxSpec, SandboxState,
    WaitOptions,
};
use sandbox_driver_host::HostProvider;
use sandbox_driver_protocol::{PluginConfig, PluginSupervisor};
use tokio::sync::OnceCell;
use tokio::time;

/// The label Petri stamps on every sandbox it creates for a run, carrying
/// the run id. Fabro's ownership of a run's sandbox is this label.
pub(crate) const PETRI_RUN_LABEL: &str = "petri.run";

/// Binary naming prefix for a plugin provider's executable: a plugin for
/// kind `e2b` is `sandbox-driver-e2b` on `PATH` unless the settings name a
/// path. The same executable serves Petri's run in the worker, which looks
/// it up under the same name.
const PLUGIN_BINARY_PREFIX: &str = "sandbox-driver";

/// Budget for the credential probe `fabro doctor` and the install flow run.
pub(crate) const DAYTONA_CREDENTIAL_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// What the server needs to reach every provider a run record can name:
/// its provider settings (which kinds are enabled, which run as plugins),
/// the Daytona credentials from the vault, and the storage root under
/// which each run's worker kept its host registry.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderAccess {
    pub(crate) providers:    ServerSandboxProvidersSettings,
    pub(crate) daytona:      Option<DaytonaCredentials>,
    /// The server's storage root. `None` where no run's host sandbox is
    /// reached (the doctor's probes, the startup inventory): a host
    /// record is then designated by path without its registry record.
    pub(crate) storage_root: Option<PathBuf>,
}

impl ProviderAccess {
    /// The host registry the run's worker kept: Petri's `host-registry`
    /// inside the run's Petri directory, which is `petri` under the run's
    /// scratch directory (the run directory `petri_runs` hands the worker).
    fn host_registry(&self, run_id: RunId) -> Option<PathBuf> {
        let root = self.storage_root.as_ref()?;
        Some(
            Storage::new(root)
                .run_scratch(&run_id)
                .root()
                .join("petri")
                .join("host-registry"),
        )
    }

    /// The settings entry for `kind`. A bundled kind without an entry is
    /// enabled with defaults; any other kind must be configured.
    fn settings_for(&self, kind: &SandboxProviderKind) -> Option<ServerSandboxProviderSettings> {
        match self.providers.get(kind) {
            Some(settings) => Some(settings.clone()),
            None if kind.bundled().is_some() => Some(ServerSandboxProviderSettings::default()),
            None => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectError {
    #[error(
        "sandbox provider `{kind}` is not configured; add [server.sandbox.providers.{kind}] to settings.toml"
    )]
    Unconfigured { kind: SandboxProviderKind },
    #[error("sandbox provider `{kind}` is disabled by server.sandbox.providers.{kind}.enabled")]
    Disabled { kind: SandboxProviderKind },
    #[error(
        "sandbox provider `{kind}` has no plugin settings; add server.sandbox.providers.{kind}"
    )]
    MissingPluginSettings { kind: SandboxProviderKind },
    #[error(
        "Daytona sandboxes require DAYTONA_API_KEY in the vault; run `fabro secret set DAYTONA_API_KEY`"
    )]
    MissingDaytonaCredentials,
    #[error("sandbox provider `{kind}` is not a valid sandbox-driver kind")]
    InvalidKind {
        kind:   SandboxProviderKind,
        #[source]
        source: sandbox_driver::InvalidIdError,
    },
    #[error("failed to connect sandbox provider `{kind}`")]
    Driver {
        kind:   SandboxProviderKind,
        #[source]
        source: sandbox_driver::Error,
    },
}

/// Connects the provider behind `kind`, unscoped: every sandbox on the
/// backend is visible to it. Callers that act on a persisted id narrow it
/// to the run, as [`attach_run_sandbox`] does.
///
/// Bundled kinds link the driver's provider crates in process. `local` is
/// the driver's Host provider with a fresh registry, enough for health;
/// a run's host sandbox is reached through the run's own registry by
/// [`attach_run_sandbox`].
/// `docker` connects to the daemon the process environment names, the
/// same variables Fabro forwards to its worker, without requiring the
/// daemon to answer: `health` reports an unreachable daemon so preflight
/// and the doctor see the cause. `daytona` needs the vault key. Any other
/// kind launches the plugin executable its settings name and supervises
/// it. Disabled entries are refused here so no caller has to remember the
/// policy check.
pub(crate) async fn connect_provider(
    kind: &SandboxProviderKind,
    access: &ProviderAccess,
) -> Result<Arc<dyn SandboxProvider>, ConnectError> {
    let settings = access
        .settings_for(kind)
        .ok_or_else(|| ConnectError::Unconfigured { kind: kind.clone() })?;
    if !settings.enabled {
        return Err(ConnectError::Disabled { kind: kind.clone() });
    }
    let driver = |source| ConnectError::Driver {
        kind: kind.clone(),
        source,
    };
    Ok(match kind.bundled() {
        Some(BundledProvider::Local) => Arc::new(HostProvider::new()),
        Some(BundledProvider::Docker) => providers::connect_docker().map_err(driver)?,
        Some(BundledProvider::Daytona) => {
            let credentials = access
                .daytona
                .as_ref()
                .ok_or(ConnectError::MissingDaytonaCredentials)?;
            providers::connect_daytona(credentials)
                .await
                .map_err(driver)?
        }
        None => {
            let plugin = settings
                .plugin
                .as_ref()
                .ok_or_else(|| ConnectError::MissingPluginSettings { kind: kind.clone() })?;
            let driver_kind = ProviderKind::try_new(kind.as_str()).map_err(|source| {
                ConnectError::InvalidKind {
                    kind: kind.clone(),
                    source,
                }
            })?;
            // The supervisor is the provider: it launches the executable now,
            // so a misconfigured plugin fails at connect time, and relaunches
            // it after a crash for new work only.
            Arc::new(
                PluginSupervisor::launch(PLUGIN_BINARY_PREFIX, plugin_config(driver_kind, plugin))
                    .await
                    .map_err(driver)?,
            )
        }
    })
}

fn plugin_config(kind: ProviderKind, settings: &SandboxPluginSettings) -> PluginConfig {
    PluginConfig {
        kind,
        path: settings.path.as_deref().map(PathBuf::from),
        sha256: settings.sha256.clone(),
        dev: settings.dev,
        args: settings.args.clone(),
        env: settings
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
        inherit_env: settings.inherit_env.clone(),
    }
}

/// Fabro's ownership of a run's sandboxes: the `petri.run` label Petri
/// stamps, with the run id.
#[must_use]
pub(crate) fn run_ownership(run_id: RunId) -> Ownership {
    Ownership::label(PETRI_RUN_LABEL, run_id.to_string())
}

/// Whether `labels` are a Petri run sandbox's: Petri's run label is
/// present, whichever run it names.
fn is_petri_sandbox(labels: &BTreeMap<String, String>) -> bool {
    labels.contains_key(PETRI_RUN_LABEL)
}

/// Attaches to a run's sandbox from its record, through the record's
/// provider scoped to the run: an attach whose sandbox does not carry the
/// run's `petri.run` label is refused. The handle is whatever state the
/// sandbox is in; [`activate`] brings it to `Running`. Deletion does not
/// come through here: a run's sandboxes are deleted through Petri's lease
/// ledger (`fabro_petri::prune`).
///
/// A host sandbox goes through [`attach_host_run_sandbox`]; every other
/// kind through its connected provider and [`attach_run_sandbox_on`].
pub(crate) async fn attach_run_sandbox(
    access: &ProviderAccess,
    record: &RunSandboxInstance,
    run_id: RunId,
) -> anyhow::Result<Arc<dyn Sandbox>> {
    if record.provider.bundled() == Some(BundledProvider::Local) {
        return attach_host_run_sandbox(access, record, run_id).await;
    }
    let provider = connect_provider(&record.provider, access)
        .await
        .with_context(|| format!("Failed to connect to the {} provider", record.provider))?;
    attach_run_sandbox_on(provider, record, run_id).await
}

/// [`attach_run_sandbox`] on an already connected, unscoped `provider` for
/// the record's kind, which attaches by the record's id: the run scope is
/// applied here.
async fn attach_run_sandbox_on(
    provider: Arc<dyn SandboxProvider>,
    record: &RunSandboxInstance,
    run_id: RunId,
) -> anyhow::Result<Arc<dyn Sandbox>> {
    let kind = &record.provider;
    let sandbox_id = &record.runtime.id;
    let provider = OwnedProvider::new(provider, run_ownership(run_id));
    let id =
        SandboxId::try_new(sandbox_id).with_context(|| format!("Invalid {kind} sandbox id"))?;
    provider
        .attach(&id, None)
        .await
        .with_context(|| format!("Failed to reconnect {kind} sandbox '{sandbox_id}'"))
}

/// Stop the sandboxes an exited worker can no longer release. This is a
/// supervisor backstop, not a deletion: the workspace and Petri's lease
/// records remain available for inspection, recovery and later pruning.
/// Local process cleanup belongs to the Host provider's worker guardian.
pub(crate) async fn stop_run_sandboxes(
    access: &ProviderAccess,
    provider: &SandboxProviderKind,
    policy: RunLifecyclePolicy,
    run_id: RunId,
) -> anyhow::Result<()> {
    if !policy.stop_on_terminal || provider.bundled() == Some(BundledProvider::Local) {
        return Ok(());
    }
    let provider = connect_provider(provider, access).await?;
    stop_run_sandboxes_on(provider, run_id).await
}

async fn stop_run_sandboxes_on(
    provider: Arc<dyn SandboxProvider>,
    run_id: RunId,
) -> anyhow::Result<()> {
    // Listing also finds a container whose create completed just before the
    // worker died without recording its id, and every nested scope. Both
    // list and attach verify ownership, so unrelated resources stay alone.
    let provider = OwnedProvider::new(provider, run_ownership(run_id));
    let sandboxes = provider
        .list(&SandboxFilter::default())
        .await
        .context("Failed to list the exited worker's sandboxes")?;
    let results =
        join_all(sandboxes.into_iter().map(|status| {
            let provider = &provider;
            async move {
                let result = match provider.attach(&status.id, None).await {
                    Ok(sandbox) => sandbox.stop().await,
                    Err(error) => Err(error),
                };
                match result {
                    // A concurrent run deletion already cleaned it up.
                    Ok(()) | Err(DriverError::NotFound { .. }) => Ok(()),
                    Err(error) => Err(anyhow::Error::new(error)
                        .context("Failed to stop an exited worker's sandbox")),
                }
            }
        }))
        .await;
    // Every owned sandbox gets its stop attempt even if one fails.
    for result in results {
        result?;
    }
    Ok(())
}

/// Attaches to a run's host sandbox: the managed directory the record
/// names, with the record the run's worker kept for it.
///
/// The worker's host registry is one process's at a time, so the server
/// observes it (`HostProvider::observe_registry`) rather than opening it:
/// the registry is read, never written, and the directory is resolved to
/// its record by path. That record carries Petri's labels, so the run's
/// `petri.run` ownership is checked here exactly as it is on Docker, and
/// the handle refuses every lifecycle change: starting, stopping, and
/// deleting the sandbox stay the worker's. A stopped sandbox's retained
/// workspace is usable through the handle.
///
/// A directory the registry does not know is designated again without a
/// record, as before this check existed: the run predates the driver that
/// records it, this process has no storage root to find the registry under
/// (a probe, a test), or the run's Petri directory is gone while the
/// directory is not. Such a handle carries no labels and so no ownership;
/// it serves the read paths only.
async fn attach_host_run_sandbox(
    access: &ProviderAccess,
    record: &RunSandboxInstance,
    run_id: RunId,
) -> anyhow::Result<Arc<dyn Sandbox>> {
    let kind = &record.provider;
    // The kind's policy (configured, enabled) is checked as for any other
    // provider; the fresh provider that connects is not what a run's
    // directory is reached through.
    connect_provider(kind, access)
        .await
        .with_context(|| format!("Failed to connect to the {kind} provider"))?;
    let sandbox_id = &record.runtime.id;
    let working_directory = &record.runtime.working_directory;
    if let Some(registry) = access.host_registry(run_id) {
        let observer: Arc<HostProvider> = Arc::new(HostProvider::observe_registry(registry));
        match observer
            .attach_directory(Path::new(working_directory), None)
            .await
        {
            Ok(handle) => {
                return OwnedProvider::new(observer, run_ownership(run_id))
                    .check(handle)
                    .await
                    .with_context(|| format!("Failed to reconnect {kind} sandbox '{sandbox_id}'"));
            }
            Err(DriverError::NotFound { .. }) => {}
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!(
                    "Failed to reconnect {kind} sandbox '{sandbox_id}' at {working_directory}"
                )));
            }
        }
    }
    let spec =
        SandboxSpec::new(SandboxSource::HostDirectory).working_directory(working_directory.clone());
    HostProvider::new()
        .create(&spec, None)
        .await
        .with_context(|| {
            format!("Failed to reconnect {kind} sandbox '{sandbox_id}' at {working_directory}")
        })
}

/// Brings a sandbox back into use, idempotently: a running sandbox is left
/// alone; a stopped or paused one is started and its Bash verified.
pub(crate) async fn activate(sandbox: &dyn Sandbox) -> sandbox_driver::Result<()> {
    let status = sandbox.describe().await?;
    if status.state == SandboxState::Running {
        return Ok(());
    }
    sandbox_driver::activate(sandbox, &WaitOptions::default()).await
}

/// Attaches to a run's sandbox and brings it to `Running`, for every
/// access-time caller. A host sandbox is returned as attached: its handle
/// works in the directory in any recorded state, and starting it is the
/// worker's alone.
pub(crate) async fn attach_running_run_sandbox(
    access: &ProviderAccess,
    record: &RunSandboxInstance,
    run_id: RunId,
) -> anyhow::Result<Arc<dyn Sandbox>> {
    let sandbox = attach_run_sandbox(access, record, run_id).await?;
    if record.provider.bundled() == Some(BundledProvider::Local) {
        return Ok(sandbox);
    }
    activate(sandbox.as_ref())
        .await
        .with_context(|| format!("Failed to start {} sandbox", record.provider))?;
    Ok(sandbox)
}

/// Whether the backend behind `kind` is reachable and the configured
/// credential accepted, for preflight. A connection failure is the
/// error; an unhealthy provider is an `Ok` report that says why.
pub(crate) async fn provider_health(
    kind: &SandboxProviderKind,
    access: &ProviderAccess,
) -> anyhow::Result<ProviderHealth> {
    let provider = connect_provider(kind, access)
        .await
        .with_context(|| format!("Failed to connect to the {kind} provider"))?;
    provider
        .health()
        .await
        .with_context(|| format!("{kind} health check failed"))
}

/// Whether the Docker daemon answers, for `fabro doctor`.
pub(crate) async fn check_docker_daemon() -> anyhow::Result<()> {
    let health = provider_health(&SandboxProviderKind::DOCKER, &ProviderAccess::default()).await?;
    match health.status {
        HealthStatus::Ok | HealthStatus::Unknown => Ok(()),
        HealthStatus::Unreachable | HealthStatus::Unauthorized => Err(anyhow::anyhow!(
            "{}",
            health
                .message
                .unwrap_or_else(|| "Failed to reach Docker daemon".to_string())
        )),
        _ => Err(anyhow::anyhow!(
            "Docker daemon reported an unknown health state"
        )),
    }
}

/// Outcome of probing a Daytona credential through the provider's health
/// check. The provider owns the list of scopes it needs and the order it
/// reports them in; Fabro only renders them.
#[derive(Debug)]
pub(crate) struct DaytonaKeyCheck {
    /// Scopes the key lacks, in Daytona's wire names.
    pub(crate) missing:  Vec<String>,
    /// Every scope the provider requires, for the remediation text.
    pub(crate) required: Vec<String>,
}

impl DaytonaKeyCheck {
    #[must_use]
    pub(crate) fn ok(&self) -> bool {
        self.missing.is_empty()
    }

    #[must_use]
    pub(crate) fn missing_display(&self) -> String {
        self.missing.join(", ")
    }

    #[must_use]
    pub(crate) fn missing_message(&self) -> String {
        format!(
            "Daytona API key is missing required scopes: {}. Regenerate the key with all \
             snapshot and sandbox scopes.",
            self.missing_display()
        )
    }

    /// Every scope the provider requires, comma separated, for remediation.
    #[must_use]
    pub(crate) fn required_display(&self) -> String {
        self.required.join(", ")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Daytona credential probe timed out after {timeout:?}")]
pub(crate) struct DaytonaCredentialProbeTimeout {
    timeout: Duration,
}

impl DaytonaCredentialProbeTimeout {
    #[must_use]
    pub(crate) const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    #[must_use]
    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// Whether `credentials` reach Daytona, are accepted, and carry the scopes
/// Fabro needs. Reachability and authentication failures are errors; a key
/// that authenticates but lacks scopes is an `Ok` check that is not `ok()`.
pub(crate) async fn check_daytona_api_key(
    credentials: &DaytonaCredentials,
    probe_timeout: Duration,
) -> anyhow::Result<DaytonaKeyCheck> {
    let access = ProviderAccess {
        providers:    ServerSandboxProvidersSettings::default(),
        daytona:      Some(credentials.clone()),
        storage_root: None,
    };
    let probe = async {
        let health = provider_health(&SandboxProviderKind::DAYTONA, &access).await?;
        match health.status {
            HealthStatus::Ok | HealthStatus::Unknown => Ok(DaytonaKeyCheck {
                missing:  Vec::new(),
                required: health.required_permissions,
            }),
            HealthStatus::Unauthorized if !health.missing_permissions.is_empty() => {
                Ok(DaytonaKeyCheck {
                    missing:  health.missing_permissions,
                    required: health.required_permissions,
                })
            }
            HealthStatus::Unauthorized => Err(anyhow::anyhow!(
                "failed to authenticate with Daytona: {}",
                health
                    .message
                    .unwrap_or_else(|| "the credential was rejected".to_string())
            )),
            _ => Err(anyhow::anyhow!(
                "failed to reach Daytona: {}",
                health
                    .message
                    .unwrap_or_else(|| "the control plane did not answer".to_string())
            )),
        }
    };
    match time::timeout(probe_timeout, probe).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::Error::new(DaytonaCredentialProbeTimeout::new(
            probe_timeout,
        ))),
    }
}

/// The sandboxes Petri created for Fabro's runs, by provider, for the
/// `/sandboxes` endpoints.
///
/// Every entry is a provider connected on first use: the inventory is
/// assembled synchronously at startup, and a provider that is down surfaces
/// as a lookup error rather than a startup failure. A listing keeps only
/// the sandboxes that carry Petri's run label, and a lookup by id answers
/// only for one that does. The `local` kind has an entry too, so a caller
/// can ask whether the kind is ready, but its sandboxes are directories the
/// run record names and there is nothing to list.
#[derive(Clone, Default)]
pub(crate) struct SandboxInventory {
    entries: Vec<Arc<InventoryEntry>>,
}

struct InventoryEntry {
    kind:       SandboxProviderKind,
    connection: Connection,
}

enum Connection {
    /// Sandboxes on this host are directories the run record names;
    /// there is nothing to list.
    HostDirectories,
    #[cfg(test)]
    Connected(Arc<dyn SandboxProvider>),
    /// Connected through [`connect_provider`] on first use.
    Lazy(Box<LazyConnection>),
}

struct LazyConnection {
    access:   ProviderAccess,
    provider: OnceCell<Arc<dyn SandboxProvider>>,
}

impl SandboxInventory {
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// A kind whose sandboxes are directories on this host: ready to run,
    /// nothing to list.
    #[must_use]
    pub(crate) fn with_host_directories(self, kind: SandboxProviderKind) -> Self {
        self.with_entry(kind, Connection::HostDirectories)
    }

    /// A provider already connected, tagged with the kind Fabro persists
    /// for it.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_connected(
        self,
        kind: SandboxProviderKind,
        provider: Arc<dyn SandboxProvider>,
    ) -> Self {
        self.with_entry(kind, Connection::Connected(provider))
    }

    /// A provider connected through `access` on first use.
    #[must_use]
    pub(crate) fn with_lazy(self, kind: SandboxProviderKind, access: ProviderAccess) -> Self {
        self.with_entry(
            kind,
            Connection::Lazy(Box::new(LazyConnection {
                access,
                provider: OnceCell::new(),
            })),
        )
    }

    fn with_entry(mut self, kind: SandboxProviderKind, connection: Connection) -> Self {
        self.entries
            .push(Arc::new(InventoryEntry { kind, connection }));
        self
    }

    /// The provider kinds this inventory covers.
    pub(crate) fn kinds(&self) -> impl Iterator<Item = &SandboxProviderKind> {
        self.entries.iter().map(|entry| &entry.kind)
    }

    pub(crate) async fn list_managed(&self) -> SandboxListResponse {
        let results = join_all(
            self.entries
                .iter()
                .map(|entry| async move { (&entry.kind, entry.list().await) }),
        )
        .await;

        let mut data = Vec::new();
        let mut provider_errors = Vec::new();
        for (kind, result) in results {
            match result {
                Ok(mut sandboxes) => data.append(&mut sandboxes),
                Err(err) => provider_errors.push(provider_error(kind.clone(), &err)),
            }
        }

        SandboxListResponse {
            data,
            meta: SandboxListMeta { provider_errors },
        }
    }

    pub(crate) async fn get_managed_by_native_id(
        &self,
        id: &str,
    ) -> Result<SandboxInfo, SandboxLookupError> {
        let results = join_all(
            self.entries
                .iter()
                .map(|entry| async move { (&entry.kind, entry.get(id).await) }),
        )
        .await;

        let mut matches = Vec::new();
        let mut provider_errors = Vec::new();
        for (kind, result) in results {
            match result {
                Ok(Some(sandbox)) => matches.push(sandbox),
                Ok(None) => {}
                Err(err) => provider_errors.push(provider_error(kind.clone(), &err)),
            }
        }

        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 if provider_errors.is_empty() => {
                Err(SandboxLookupError::NotFound { id: id.to_string() })
            }
            0 => Err(SandboxLookupError::ProviderUnavailable {
                id: id.to_string(),
                provider_errors,
            }),
            _ => Err(SandboxLookupError::Conflict {
                id:        id.to_string(),
                providers: matches
                    .into_iter()
                    .map(|sandbox| sandbox.provider)
                    .collect(),
            }),
        }
    }
}

impl InventoryEntry {
    /// The provider, connected on first use; `None` when the kind has
    /// nothing to list.
    async fn provider(&self) -> anyhow::Result<Option<&Arc<dyn SandboxProvider>>> {
        match &self.connection {
            Connection::HostDirectories => Ok(None),
            #[cfg(test)]
            Connection::Connected(provider) => Ok(Some(provider)),
            Connection::Lazy(lazy) => lazy
                .provider
                .get_or_try_init(|| async {
                    connect_provider(&self.kind, &lazy.access)
                        .await
                        .with_context(|| format!("Failed to connect to the {} provider", self.kind))
                })
                .await
                .map(Some),
        }
    }

    async fn list(&self) -> anyhow::Result<Vec<SandboxInfo>> {
        let Some(provider) = self.provider().await? else {
            return Ok(Vec::new());
        };
        // The driver filters on a label's value; Petri's run label is a
        // different run id on every sandbox, so the listing is narrowed to
        // the key here.
        let statuses = provider
            .list(&SandboxFilter::default())
            .await
            .with_context(|| format!("Failed to list {} sandboxes", self.kind))?;
        Ok(statuses
            .into_iter()
            .filter(|status| is_petri_sandbox(&status.labels))
            .map(|status| SandboxInfo {
                provider: self.kind.clone(),
                status,
            })
            .collect())
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<SandboxInfo>> {
        let Some(provider) = self.provider().await? else {
            return Ok(None);
        };
        // An id the driver cannot even name is not one of ours.
        let Ok(sandbox_id) = SandboxId::try_new(id) else {
            return Ok(None);
        };
        let handle = match provider.attach(&sandbox_id, None).await {
            Ok(handle) => handle,
            Err(DriverError::NotFound { .. }) => return Ok(None),
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("Failed to look up {} sandbox '{id}'", self.kind)));
            }
        };
        let status = handle
            .describe()
            .await
            .with_context(|| format!("Failed to describe {} sandbox '{id}'", self.kind))?;
        // Unknown to the provider, deleted, or not a run's: none is in the
        // inventory.
        if status.state == SandboxState::Deleted || !is_petri_sandbox(&status.labels) {
            return Ok(None);
        }
        Ok(Some(SandboxInfo {
            provider: self.kind.clone(),
            status,
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SandboxLookupError {
    #[error("sandbox '{id}' was not found by any configured provider")]
    NotFound { id: String },
    #[error("sandbox '{id}' matched more than one configured provider")]
    Conflict {
        id:        String,
        providers: Vec<SandboxProviderKind>,
    },
    #[error("sandbox '{id}' could not be found definitively because one or more providers failed")]
    ProviderUnavailable {
        id:              String,
        provider_errors: Vec<SandboxProviderLookupError>,
    },
}

fn provider_error(
    provider: SandboxProviderKind,
    err: &anyhow::Error,
) -> SandboxProviderLookupError {
    SandboxProviderLookupError {
        provider,
        message: err
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": "),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Scripted providers holding Petri-labelled sandboxes, for the
    //! inventory and the attach path.

    use std::sync::Arc;

    use sandbox_driver::{SandboxProvider, SandboxState};
    use sandbox_driver_testing::{ScriptedProvider, ScriptedSandbox};

    use super::PETRI_RUN_LABEL;

    /// A running scripted sandbox Petri created for `run_id`, so a scoped
    /// attach accepts it and the inventory lists it.
    #[must_use]
    pub(crate) fn petri_scripted_sandbox(id: &str, run_id: &str) -> Arc<ScriptedSandbox> {
        Arc::new(
            ScriptedSandbox::with_id_and_working_dir(id, "/workspace")
                .state(SandboxState::Running)
                .label(PETRI_RUN_LABEL, run_id),
        )
    }

    /// A scripted provider of `kind` holding `sandboxes`.
    #[must_use]
    pub(crate) fn scripted_provider(
        kind: &str,
        sandboxes: Vec<Arc<ScriptedSandbox>>,
    ) -> Arc<dyn SandboxProvider> {
        let provider = ScriptedProvider::new(kind);
        for sandbox in sandboxes {
            provider.register(sandbox);
        }
        Arc::new(provider)
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::RunSandboxRuntime;
    use fabro_types::settings::server::SandboxPluginSettings;
    use sandbox_driver::SandboxProvider as _;
    use sandbox_driver_testing::ScriptedSandbox;

    use super::test_support::{petri_scripted_sandbox, scripted_provider};
    use super::*;

    fn kind(name: &str) -> SandboxProviderKind {
        SandboxProviderKind::try_new(name).expect("valid kind")
    }

    fn record(provider: SandboxProviderKind, id: &str) -> RunSandboxInstance {
        RunSandboxInstance {
            provider,
            image: None,
            snapshot: None,
            runtime: RunSandboxRuntime {
                id:                id.to_string(),
                working_directory: "/workspace".to_string(),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
                workspace_root:    None,
                repos_root:        None,
                primary_repo_path: None,
                primary_repo_link: None,
            },
            ready_duration_ms: None,
            retained: None,
        }
    }

    /// A plugin kind whose executable does not exist, so every connection
    /// fails.
    fn unreachable_plugin_access(name: &str) -> ProviderAccess {
        let mut providers = ServerSandboxProvidersSettings::default();
        providers
            .entries
            .insert(kind(name), ServerSandboxProviderSettings {
                enabled: true,
                plugin:  Some(SandboxPluginSettings {
                    path: Some(format!("/nonexistent/sandbox-driver-{name}")),
                    dev: true,
                    ..SandboxPluginSettings::default()
                }),
            });
        ProviderAccess {
            providers,
            daytona: None,
            storage_root: None,
        }
    }

    #[test]
    fn ownership_is_keyed_on_petris_run_label() {
        let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
        let ownership = run_ownership(run_id);
        assert_eq!(ownership.labels().iter().collect::<Vec<_>>(), vec![(
            &"petri.run".to_string(),
            &run_id.to_string()
        )]);
        let mut labels = BTreeMap::new();
        assert!(!ownership.owns(&labels));
        labels.insert("sh.fabro.managed".to_string(), "true".to_string());
        assert!(
            !ownership.owns(&labels),
            "Fabro's old labels prove nothing; Petri stamps petri.*"
        );
        labels.insert("petri.run".to_string(), "another-run".to_string());
        assert!(!ownership.owns(&labels));
        labels.insert("petri.run".to_string(), run_id.to_string());
        assert!(ownership.owns(&labels));
    }

    #[tokio::test]
    async fn worker_cleanup_stops_all_owned_sandboxes_without_deleting_their_data() {
        let run_id = RunId::new();
        let mine = petri_scripted_sandbox("root", &run_id.to_string());
        let nested = petri_scripted_sandbox("nested", &run_id.to_string());
        let theirs = petri_scripted_sandbox("foreign", &RunId::new().to_string());
        let unlabelled = Arc::new(ScriptedSandbox::new());
        mine.fs().write("retained", b"keep me").await.unwrap();
        let provider = scripted_provider("docker", vec![
            Arc::clone(&mine),
            Arc::clone(&nested),
            Arc::clone(&theirs),
            Arc::clone(&unlabelled),
        ]);

        stop_run_sandboxes_on(provider, run_id).await.unwrap();

        for sandbox in [&mine, &nested] {
            assert_eq!(sandbox.current_state(), SandboxState::Stopped);
            assert_eq!(sandbox.delete_count(), 0);
            assert_eq!(sandbox.start_count(), 0);
        }
        assert_eq!(mine.fs().read("retained").await.unwrap(), b"keep me");
        for sandbox in [&theirs, &unlabelled] {
            assert_eq!(sandbox.current_state(), SandboxState::Running);
            assert_eq!(sandbox.stop_count(), 0);
            assert_eq!(sandbox.delete_count(), 0);
        }
    }

    #[tokio::test]
    async fn worker_cleanup_skips_keep_running_and_host_environments() {
        // This plugin cannot connect: skipping the policy must happen first.
        let provider = kind("unreachable-cleanup");
        let access = unreachable_plugin_access(provider.as_str());
        for preserve in [false, true] {
            stop_run_sandboxes(
                &access,
                &provider,
                RunLifecyclePolicy {
                    preserve,
                    stop_on_terminal: false,
                },
                RunId::new(),
            )
            .await
            .unwrap();
            stop_run_sandboxes(
                &access,
                &SandboxProviderKind::LOCAL,
                RunLifecyclePolicy {
                    preserve,
                    stop_on_terminal: true,
                },
                RunId::new(),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn a_scoped_provider_attaches_only_to_the_runs_sandbox() {
        let run_id = RunId::new();
        let other_run = RunId::new();
        let provider = scripted_provider("docker", vec![
            petri_scripted_sandbox("mine", &run_id.to_string()),
            petri_scripted_sandbox("theirs", &other_run.to_string()),
            Arc::new(
                ScriptedSandbox::with_id_and_working_dir("unlabelled", "/work")
                    .state(SandboxState::Running),
            ),
        ]);
        let scoped = OwnedProvider::new(provider, run_ownership(run_id));

        let mine = scoped
            .attach(&SandboxId::try_new("mine").unwrap(), None)
            .await
            .expect("the run's own sandbox attaches");
        assert_eq!(mine.id().as_str(), "mine");
        for foreign in ["theirs", "unlabelled"] {
            let error = scoped
                .attach(&SandboxId::try_new(foreign).unwrap(), None)
                .await
                .err()
                .expect("a sandbox without the run's label is refused");
            assert!(
                matches!(error, DriverError::NotOwned { .. }),
                "{foreign}: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn attach_run_sandbox_keys_ownership_on_petris_run_label() {
        let run_id = RunId::new();
        let other_run = RunId::new();
        // Petri's own sandbox for the run, another run's, and one that carries
        // only Fabro's retired labels.
        let fabro_labelled = Arc::new(
            ScriptedSandbox::with_id_and_working_dir("fabro-era", "/workspace")
                .state(SandboxState::Running)
                .label("sh.fabro.managed", "true")
                .label("sh.fabro.run_id", run_id.to_string()),
        );
        let provider = scripted_provider("docker", vec![
            petri_scripted_sandbox("petri-container", &run_id.to_string()),
            petri_scripted_sandbox("other-runs-container", &other_run.to_string()),
            fabro_labelled,
        ]);

        let attached = attach_run_sandbox_on(
            Arc::clone(&provider),
            &record(SandboxProviderKind::DOCKER, "petri-container"),
            run_id,
        )
        .await
        .expect("the container Petri labelled with the run attaches");
        assert_eq!(attached.id().as_str(), "petri-container");

        for foreign in ["other-runs-container", "fabro-era"] {
            let error = attach_run_sandbox_on(
                Arc::clone(&provider),
                &record(SandboxProviderKind::DOCKER, foreign),
                run_id,
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("{foreign} does not carry petri.run={run_id}"));
            assert!(
                error.chain().any(|cause| matches!(
                    cause.downcast_ref::<DriverError>(),
                    Some(DriverError::NotOwned { .. })
                )),
                "{foreign}: {error:#}"
            );
        }
    }

    #[tokio::test]
    async fn daytona_requires_explicit_credentials() {
        let error = connect_provider(&SandboxProviderKind::DAYTONA, &ProviderAccess::default())
            .await
            .err()
            .expect("daytona must not fall back to the environment");
        assert!(matches!(error, ConnectError::MissingDaytonaCredentials));
    }

    #[tokio::test]
    async fn disabled_and_unconfigured_kinds_are_refused_before_any_connection() {
        let mut providers = ServerSandboxProvidersSettings::default();
        providers
            .entries
            .insert(SandboxProviderKind::DOCKER, ServerSandboxProviderSettings {
                enabled: false,
                plugin:  None,
            });
        let access = ProviderAccess {
            providers,
            daytona: None,
            storage_root: None,
        };
        let error = connect_provider(&SandboxProviderKind::DOCKER, &access)
            .await
            .err()
            .expect("a disabled provider must not connect");
        assert!(
            matches!(error, ConnectError::Disabled { kind } if kind == SandboxProviderKind::DOCKER)
        );

        let error = connect_provider(&kind("e2b"), &ProviderAccess::default())
            .await
            .err()
            .expect("a plugin kind without settings cannot launch");
        assert!(matches!(error, ConnectError::Unconfigured { kind: k } if k == kind("e2b")));
    }

    /// The worker's registry knows the run's directory: the server attaches
    /// to it with the worker's record, checks the run's label on it, and
    /// works in it in whatever state the worker left it.
    #[tokio::test]
    async fn a_host_record_attaches_with_the_workers_managed_record() {
        let storage = tempfile::tempdir().unwrap();
        let access = ProviderAccess {
            storage_root: Some(storage.path().to_path_buf()),
            ..ProviderAccess::default()
        };
        let run_id = RunId::new();
        let registry = access.host_registry(run_id).expect("a storage root");
        // The worker's provider over the run's registry, creating the scope's
        // managed workspace with Petri's labels.
        let worker = HostProvider::with_registry(&registry)
            .await
            .expect("the worker opens the run's registry");
        let workspace = registry
            .parent()
            .expect("the run's Petri directory")
            .join("scopes")
            .join("ws")
            .join("work");
        let mut spec = SandboxSpec::new(SandboxSource::HostDirectory)
            .working_directory(workspace.display().to_string());
        spec.workspace_ownership = Some(sandbox_driver::WorkspaceOwnership::Managed);
        spec.labels
            .insert(PETRI_RUN_LABEL.to_owned(), run_id.to_string());
        let created = worker
            .create(&spec, None)
            .await
            .expect("the worker creates the managed workspace");
        created
            .fs()
            .write("note", b"kept")
            .await
            .expect("the worker writes");
        let mut record = record(SandboxProviderKind::LOCAL, created.id().as_str());
        record.runtime.working_directory = created.working_directory().to_owned();

        let attached = attach_run_sandbox(&access, &record, run_id)
            .await
            .expect("the run's directory attaches with its record");
        assert_eq!(
            attached.id(),
            created.id(),
            "the worker's id, not a derived one"
        );
        let status = attached.describe().await.expect("describe");
        assert_eq!(
            status.labels.get(PETRI_RUN_LABEL).map(String::as_str),
            Some(run_id.to_string().as_str())
        );
        assert_eq!(
            status.workspace_ownership,
            Some(sandbox_driver::WorkspaceOwnership::Managed)
        );

        // A directory the registry records for another run is refused, as
        // on Docker: the record's label, not the directory, is the proof.
        let other_run = RunId::new();
        let mut foreign_spec = SandboxSpec::new(SandboxSource::HostDirectory).working_directory(
            registry
                .parent()
                .expect("the run's Petri directory")
                .join("scopes")
                .join("foreign")
                .join("work")
                .display()
                .to_string(),
        );
        foreign_spec.workspace_ownership = Some(sandbox_driver::WorkspaceOwnership::Managed);
        foreign_spec
            .labels
            .insert(PETRI_RUN_LABEL.to_owned(), other_run.to_string());
        let foreign = worker
            .create(&foreign_spec, None)
            .await
            .expect("another run's workspace in the same registry");
        let mut foreign_record = record.clone();
        foreign_record.runtime.working_directory = foreign.working_directory().to_owned();
        let error = attach_run_sandbox(&access, &foreign_record, run_id)
            .await
            .err()
            .expect("the directory carries another run's label");
        assert!(
            error.chain().any(|cause| matches!(
                cause.downcast_ref::<DriverError>(),
                Some(DriverError::NotOwned { .. })
            )),
            "{error:#}"
        );

        // The worker stops the sandbox at release; the retained workspace is
        // still read through the server's handle, without starting it.
        created.stop().await.expect("the worker stops");
        let running = attach_running_run_sandbox(&access, &record, run_id)
            .await
            .expect("a stopped host sandbox's workspace is usable");
        assert_eq!(
            running.describe().await.expect("describe").state,
            SandboxState::Stopped,
            "the worker's state shows through"
        );
        assert_eq!(running.fs().read("note").await.expect("read"), b"kept");
        let refused = running
            .delete()
            .await
            .expect_err("deletion is the worker's");
        assert!(matches!(refused, DriverError::ReadOnly { .. }), "{refused}");

        // A directory the registry does not know is designated as before.
        let other = tempfile::tempdir().unwrap();
        let mut unknown = record.clone();
        unknown.runtime.working_directory =
            other.path().canonicalize().unwrap().display().to_string();
        let designated = attach_run_sandbox(&access, &unknown, run_id)
            .await
            .expect("an unrecorded directory is designated");
        let status = designated.describe().await.expect("describe");
        assert_eq!(
            status.workspace_ownership,
            Some(sandbox_driver::WorkspaceOwnership::Designated)
        );
        assert!(status.labels.is_empty());

        created.delete().await.expect("the worker deletes");
        foreign.delete().await.expect("the worker deletes");
    }

    /// Without a storage root there is no registry to consult: the directory
    /// is designated again, whatever id the record carries.
    #[tokio::test]
    async fn a_host_record_reconnects_by_designating_its_directory() {
        let directory = tempfile::tempdir().unwrap();
        let working_directory = directory
            .path()
            .canonicalize()
            .unwrap()
            .display()
            .to_string();
        // An id no registry of this process knows, as a worker mints for a
        // long path.
        let mut record = record(SandboxProviderKind::LOCAL, "host-0123456789abcdef");
        record.runtime.working_directory = working_directory.clone();

        let sandbox = attach_run_sandbox(&ProviderAccess::default(), &record, RunId::new())
            .await
            .expect("the directory is designated again");
        assert_eq!(sandbox.working_directory(), working_directory);
        activate(sandbox.as_ref())
            .await
            .expect("a host sandbox runs");
        assert!(directory.path().is_dir());

        // The path-derived id designates the same directory.
        let derived = HostProvider::directory_id(directory.path())
            .await
            .expect("an id for the directory");
        record.runtime.id = derived.to_string();
        let sandbox = attach_run_sandbox(&ProviderAccess::default(), &record, RunId::new())
            .await
            .expect("the derived id attaches");
        assert_eq!(sandbox.id(), &derived);
    }

    #[test]
    fn plugin_config_carries_every_launch_setting() {
        let config = plugin_config(
            ProviderKind::try_new("e2b").unwrap(),
            &SandboxPluginSettings {
                path:        Some("/opt/e2b".to_string()),
                sha256:      Some("abc".to_string()),
                dev:         true,
                args:        vec!["--flag".to_string()],
                env:         BTreeMap::from([("A".to_string(), "1".to_string())]),
                inherit_env: vec!["PATH".to_string()],
            },
        );
        assert_eq!(
            config.path.as_deref(),
            Some(std::path::Path::new("/opt/e2b"))
        );
        assert_eq!(config.sha256.as_deref(), Some("abc"));
        assert!(config.dev);
        assert_eq!(config.args, vec!["--flag"]);
        assert_eq!(config.env.get("A").map(String::as_str), Some("1"));
        assert_eq!(config.inherit_env, vec!["PATH"]);
    }

    #[test]
    fn missing_scopes_render_as_the_provider_reports_them() {
        let check = DaytonaKeyCheck {
            missing:  vec!["write:snapshots".to_string(), "write:sandboxes".to_string()],
            required: vec![
                "write:snapshots".to_string(),
                "delete:snapshots".to_string(),
                "write:sandboxes".to_string(),
                "delete:sandboxes".to_string(),
            ],
        };
        assert!(!check.ok());
        assert_eq!(check.missing_display(), "write:snapshots, write:sandboxes");
        assert_eq!(
            check.missing_message(),
            "Daytona API key is missing required scopes: write:snapshots, write:sandboxes. \
             Regenerate the key with all snapshot and sandbox scopes."
        );
        assert_eq!(
            check.required_display(),
            "write:snapshots, delete:snapshots, write:sandboxes, delete:sandboxes"
        );
    }

    #[tokio::test]
    async fn credential_probe_reports_configured_timeout() {
        // A non-routable address: the probe cannot finish within the budget.
        let credentials = DaytonaCredentials::new("dtn_test".to_string())
            .with_api_url(Some("http://10.255.255.1:1/api".to_string()));
        let err = check_daytona_api_key(&credentials, Duration::from_millis(1))
            .await
            .expect_err("probe should time out");
        let timeout = err
            .downcast_ref::<DaytonaCredentialProbeTimeout>()
            .expect("timeout should preserve its type");
        assert_eq!(timeout.timeout(), Duration::from_millis(1));
        assert_eq!(
            err.to_string(),
            "Daytona credential probe timed out after 1ms"
        );
    }

    #[tokio::test]
    async fn the_inventory_lists_petris_sandboxes_across_providers() {
        let foreign = Arc::new(
            ScriptedSandbox::with_id_and_working_dir("someone-elses", "/work")
                .state(SandboxState::Running),
        );
        let inventory = SandboxInventory::empty()
            .with_host_directories(SandboxProviderKind::LOCAL)
            .with_connected(
                SandboxProviderKind::DOCKER,
                scripted_provider("docker", vec![
                    petri_scripted_sandbox("docker-1", "run-1"),
                    foreign,
                ]),
            )
            .with_connected(
                SandboxProviderKind::DAYTONA,
                scripted_provider("daytona", vec![petri_scripted_sandbox(
                    "daytona-1",
                    "run-2",
                )]),
            );

        let response = inventory.list_managed().await;

        let mut ids: Vec<_> = response.data.iter().map(|s| s.status.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["daytona-1", "docker-1"]);
        assert!(response.meta.provider_errors.is_empty());
        let kinds: Vec<_> = inventory.kinds().cloned().collect();
        assert_eq!(kinds, [
            SandboxProviderKind::LOCAL,
            SandboxProviderKind::DOCKER,
            SandboxProviderKind::DAYTONA
        ]);

        let found = inventory
            .get_managed_by_native_id("daytona-1")
            .await
            .expect("one provider matches");
        assert_eq!(found.provider, SandboxProviderKind::DAYTONA);
        let error = inventory
            .get_managed_by_native_id("someone-elses")
            .await
            .expect_err("a sandbox without Petri's label is not in the inventory");
        assert!(matches!(error, SandboxLookupError::NotFound { .. }));
    }

    #[tokio::test]
    async fn the_inventory_reports_a_provider_that_cannot_connect_beside_the_others() {
        let inventory = SandboxInventory::empty()
            .with_connected(
                SandboxProviderKind::DOCKER,
                scripted_provider("docker", vec![petri_scripted_sandbox("docker-1", "run-1")]),
            )
            .with_lazy(kind("e2b"), unreachable_plugin_access("e2b"));

        let response = inventory.list_managed().await;
        assert_eq!(response.data.len(), 1);
        assert_eq!(response.meta.provider_errors.len(), 1);
        assert_eq!(response.meta.provider_errors[0].provider, kind("e2b"));
        assert!(
            response.meta.provider_errors[0]
                .message
                .contains("Failed to connect to the e2b provider"),
            "{}",
            response.meta.provider_errors[0].message
        );

        let error = inventory
            .get_managed_by_native_id("maybe-missing")
            .await
            .expect_err("the failed provider may have held it");
        assert!(matches!(
            error,
            SandboxLookupError::ProviderUnavailable { .. }
        ));
    }

    #[tokio::test]
    async fn the_inventory_reports_a_conflict_when_two_providers_match() {
        let inventory = SandboxInventory::empty()
            .with_connected(
                SandboxProviderKind::DOCKER,
                scripted_provider("docker", vec![petri_scripted_sandbox("same-id", "run-1")]),
            )
            .with_connected(
                SandboxProviderKind::DAYTONA,
                scripted_provider("daytona", vec![petri_scripted_sandbox("same-id", "run-1")]),
            );

        let error = inventory
            .get_managed_by_native_id("same-id")
            .await
            .expect_err("two providers match");

        let SandboxLookupError::Conflict { providers, .. } = error else {
            panic!("expected a conflict, got {error:?}");
        };
        assert_eq!(providers, [
            SandboxProviderKind::DOCKER,
            SandboxProviderKind::DAYTONA
        ]);
    }
}
