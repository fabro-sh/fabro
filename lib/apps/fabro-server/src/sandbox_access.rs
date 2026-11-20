//! The server's direct access to run sandboxes through sandbox-driver.
//!
//! Petri creates every run sandbox and records it: the provider, the
//! provider's id and the working directory travel on `scope.acquired` into
//! the run's [`RunSandboxInstance`], and every Docker or Daytona sandbox
//! carries the `petri.run` label with the run id. The server reaches a
//! run's sandbox for the sandbox tab, the terminal, SSH, preview URLs, VNC,
//! `fabro cp` and Ask Fabro by opening its own sandbox catalog and attaching
//! to the record's reference, without going through Petri.
//!
//! Ownership is keyed on `petri.run`: a persisted id is acted on only when
//! the sandbox behind it still carries the run's label, so an id that has
//! come to name someone else's sandbox on a shared daemon is refused.
//!
//! TODO(parity): a host sandbox is the record's working directory,
//! designated again, with no ownership check: sandbox-driver's built-in
//! Host provider serves its owning process only and keeps no record the
//! server could check the run's label against.
//!
//! Credentials arrive explicitly. Nothing here reads the process
//! environment for a secret: the Daytona key comes from the vault through
//! [`DaytonaCredentials`]. The Docker entry names the daemon this process's
//! `DOCKER_HOST` names, the same variables Fabro forwards to its worker, so
//! the server and the run's containers meet on one daemon. A third-party
//! provider kind is the operator's plugin entry, launched by sandbox-driver
//! over protocol 3.

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
pub(crate) use fabro_petri::providers::DaytonaCredentials;
use fabro_petri::providers::{self, SandboxProviderConfig};
use fabro_types::settings::run::RunEnvironmentSettings;
use fabro_types::settings::server::{
    SandboxPluginSettings, ServerSandboxProviderSettings, ServerSandboxProvidersSettings,
};
use fabro_types::{
    BundledProvider, RunId, RunSandboxInstance, SandboxInfo, SandboxListMeta, SandboxListResponse,
    SandboxProviderKind, SandboxProviderLookupError, SandboxStatus,
};
use futures_util::future::join_all;
use sandbox_driver::{
    Descriptor, Error as DriverError, Health, LabelFilter, PluginTrust, ProviderEntry,
    ProviderKind, ProviderName, Sandbox, SandboxId, SandboxRef, SandboxSpec, SandboxState,
    Sandboxes, Sha256Digest, Source, WorkspaceOwnership,
};
use tokio::sync::OnceCell;
use tokio::time;

/// The label Petri stamps on every sandbox it creates for a run, carrying
/// the run id. Fabro's ownership of a run's sandbox is this label.
pub(crate) const PETRI_RUN_LABEL: &str = "petri.run";

/// Binary naming prefix for a plugin provider's executable: a plugin for
/// kind `e2b` is `sandbox-driver-e2b` on `PATH` unless the settings name a
/// path.
const PLUGIN_BINARY_PREFIX: &str = "sandbox-driver";

/// Budget for the credential probe `fabro doctor` and the install flow run.
pub(crate) const DAYTONA_CREDENTIAL_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// What the server needs to reach every provider a run record can name:
/// its provider settings (which kinds are enabled, which run as plugins)
/// and the sandbox configuration, with the Daytona credentials from the
/// vault.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderAccess {
    pub(crate) providers: ServerSandboxProvidersSettings,
    pub(crate) sandbox:   SandboxProviderConfig,
}

impl ProviderAccess {
    /// The settings entry for `kind`. A bundled kind without an entry is
    /// enabled with defaults; any other kind must be configured.
    fn settings_for(&self, kind: &SandboxProviderKind) -> Option<ServerSandboxProviderSettings> {
        match self.providers.get(kind) {
            Some(settings) => Some(settings.clone()),
            None if kind.bundled().is_some() => Some(ServerSandboxProviderSettings::default()),
            None => None,
        }
    }

    /// The catalog entry `kind` is reached through, after its policy check:
    /// configured, enabled, and for Daytona a vault key. Disabled entries
    /// are refused here so no caller has to remember the policy check.
    fn entry_for(&self, kind: &SandboxProviderKind) -> Result<ProviderName, ConnectError> {
        let settings = self
            .settings_for(kind)
            .ok_or_else(|| ConnectError::Unconfigured { kind: kind.clone() })?;
        if !settings.enabled {
            return Err(ConnectError::Disabled { kind: kind.clone() });
        }
        let name = match kind.bundled() {
            Some(BundledProvider::Local) => "host",
            Some(BundledProvider::Docker) => "docker",
            Some(BundledProvider::Daytona) => {
                if self.sandbox.daytona().is_none() {
                    return Err(ConnectError::MissingDaytonaCredentials);
                }
                "daytona"
            }
            None => {
                if settings.plugin.is_none() {
                    return Err(ConnectError::MissingPluginSettings { kind: kind.clone() });
                }
                kind.as_str()
            }
        };
        ProviderName::try_new(name).map_err(|source| ConnectError::InvalidKind {
            kind: kind.clone(),
            source,
        })
    }

    /// The server's catalog: the built-in entries and an entry per enabled
    /// plugin kind.
    fn open(&self) -> Result<Sandboxes, ConnectError> {
        let mut plugins = Vec::new();
        for (kind, settings) in &self.providers.entries {
            let (None, true, Some(plugin)) = (kind.bundled(), settings.enabled, &settings.plugin)
            else {
                continue;
            };
            let invalid = |source| ConnectError::InvalidKind {
                kind: kind.clone(),
                source,
            };
            let name = ProviderName::try_new(kind.as_str()).map_err(invalid)?;
            let plugin_kind = ProviderKind::try_new(kind.as_str()).map_err(invalid)?;
            plugins.push((name, plugin_entry(plugin_kind, plugin)?));
        }
        providers::open_catalog(&self.sandbox, plugins).map_err(|source| ConnectError::Driver {
            kind: SandboxProviderKind::LOCAL,
            source,
        })
    }
}

/// A plugin kind's catalog entry from its settings. Without a configured
/// path the executable is `sandbox-driver-<kind>` on `PATH`.
fn plugin_entry(
    kind: ProviderKind,
    settings: &SandboxPluginSettings,
) -> Result<ProviderEntry, ConnectError> {
    let path = match &settings.path {
        Some(path) => Some(path.clone()),
        None => find_on_path(&format!("{PLUGIN_BINARY_PREFIX}-{kind}"))
            .map(|path| path.display().to_string()),
    };
    let trust = match (&settings.sha256, settings.dev) {
        (Some(digest), _) => Some(PluginTrust::Sha256 {
            digest: Sha256Digest::try_new(digest.as_str()).map_err(|source| {
                ConnectError::InvalidChecksum {
                    kind: kind.to_string(),
                    source,
                }
            })?,
        }),
        (None, true) => Some(PluginTrust::Development),
        (None, false) => None,
    };
    Ok(ProviderEntry {
        plugin: kind,
        config: BTreeMap::new(),
        secrets: Vec::new(),
        path,
        trust,
        args: settings.args.clone(),
        env: settings.env.clone(),
        inherit_env: Some(settings.inherit_env.clone()),
    })
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    #[expect(
        clippy::disallowed_methods,
        reason = "a plugin without a configured path is found on this process's PATH"
    )]
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(binary))
        .find(|candidate| candidate.is_file())
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
    #[error("sandbox provider `{kind}` has an invalid sha256 checksum")]
    InvalidChecksum {
        kind:   String,
        #[source]
        source: sandbox_driver::InvalidIdError,
    },
    #[error("failed to open the sandbox catalog")]
    Driver {
        kind:   SandboxProviderKind,
        #[source]
        source: DriverError,
    },
}

/// Whether `labels` are a Petri run sandbox's: Petri's run label is
/// present, whichever run it names.
fn is_petri_sandbox(labels: &BTreeMap<String, String>) -> bool {
    labels.contains_key(PETRI_RUN_LABEL)
}

/// A run's sandbox and its provider's descriptor, which says what the
/// sandbox supports: its isolation, which ports it exposes, its endpoints.
pub(crate) struct RunSandbox {
    pub(crate) sandbox:    Arc<dyn Sandbox>,
    pub(crate) descriptor: Descriptor,
}

impl RunSandbox {
    /// The driver's status, as the API shows it.
    pub(crate) async fn status(&self) -> Result<SandboxStatus, DriverError> {
        let status = self.sandbox.status().await?;
        Ok(SandboxStatus::from_driver(
            &status,
            Some(self.descriptor.isolation),
        ))
    }
}

/// Attaches to a run's sandbox from its record, refused unless the sandbox
/// carries the run's `petri.run` label. The handle is whatever state the
/// sandbox is in. Deletion does not come through here: a run's sandboxes
/// are deleted through Petri's lease ledger (`fabro_petri::prune`).
///
/// A host sandbox is the record's working directory, designated again.
pub(crate) async fn attach_run_sandbox(
    access: &ProviderAccess,
    record: &RunSandboxInstance,
    run_id: RunId,
) -> anyhow::Result<RunSandbox> {
    let kind = &record.provider;
    let entry = access
        .entry_for(kind)
        .with_context(|| format!("Failed to connect to the {kind} provider"))?;
    let sandboxes = access.open()?;
    if kind.bundled() == Some(BundledProvider::Local) {
        return designate_host_directory(&sandboxes, &entry, record).await;
    }
    attach_owned(&sandboxes, &entry, record, run_id).await
}

/// [`attach_run_sandbox`] for a record on catalog `entry` of `sandboxes`.
async fn attach_owned(
    sandboxes: &Sandboxes,
    entry: &ProviderName,
    record: &RunSandboxInstance,
    run_id: RunId,
) -> anyhow::Result<RunSandbox> {
    let kind = &record.provider;
    let sandbox_id = &record.runtime.id;
    let descriptor = describe(sandboxes, entry, kind).await?;
    let reference = reference_in(&descriptor, entry, kind, sandbox_id)?;
    let handle: Arc<dyn Sandbox> = Arc::from(
        sandboxes
            .attach(&reference)
            .await
            .with_context(|| format!("Failed to reconnect {kind} sandbox '{sandbox_id}'"))?,
    );
    let status = handle
        .status()
        .await
        .with_context(|| format!("Failed to read {kind} sandbox '{sandbox_id}'"))?;
    if status.labels.get(PETRI_RUN_LABEL).map(String::as_str) != Some(run_id.to_string().as_str()) {
        anyhow::bail!("{kind} sandbox '{sandbox_id}' does not belong to run {run_id}");
    }
    Ok(RunSandbox {
        sandbox: handle,
        descriptor,
    })
}

/// The descriptor of catalog `entry`, connecting it if needed.
async fn describe(
    sandboxes: &Sandboxes,
    entry: &ProviderName,
    kind: &SandboxProviderKind,
) -> anyhow::Result<Descriptor> {
    sandboxes
        .provider(entry.as_str())?
        .describe()
        .await
        .with_context(|| format!("Failed to connect to the {kind} provider"))
}

/// The reference of `sandbox_id` on `entry`, in the namespace `descriptor`
/// reports.
fn reference_in(
    descriptor: &Descriptor,
    entry: &ProviderName,
    kind: &SandboxProviderKind,
    sandbox_id: &str,
) -> anyhow::Result<SandboxRef> {
    Ok(SandboxRef {
        provider:  entry.clone(),
        namespace: descriptor.namespace.id.clone(),
        id:        SandboxId::try_new(sandbox_id)
            .with_context(|| format!("Invalid {kind} sandbox id"))?,
    })
}

/// A run's host sandbox: its recorded working directory, designated again
/// on the built-in Host provider, which never removes it.
async fn designate_host_directory(
    sandboxes: &Sandboxes,
    entry: &ProviderName,
    record: &RunSandboxInstance,
) -> anyhow::Result<RunSandbox> {
    let kind = &record.provider;
    let descriptor = describe(sandboxes, entry, kind).await?;
    let working_directory = &record.runtime.working_directory;
    let mut spec = SandboxSpec::new(Source::HostDirectory);
    spec.working_directory = Some(working_directory.clone());
    spec.workspace_ownership = Some(WorkspaceOwnership::Designated);
    let handle = sandboxes
        .builder("host")
        .await?
        .spec(spec)
        .create()
        .await
        .with_context(|| format!("Failed to reconnect {kind} sandbox at {working_directory}"))?;
    Ok(RunSandbox {
        sandbox: Arc::from(handle),
        descriptor,
    })
}

/// Attaches to a run's sandbox and brings it to `Running`, for every
/// access-time caller. A host sandbox is returned as designated: its
/// directory is usable in any recorded state.
pub(crate) async fn attach_running_run_sandbox(
    access: &ProviderAccess,
    record: &RunSandboxInstance,
    run_id: RunId,
) -> anyhow::Result<RunSandbox> {
    let sandbox = attach_run_sandbox(access, record, run_id).await?;
    if record.provider.bundled() == Some(BundledProvider::Local) {
        return Ok(sandbox);
    }
    activate(sandbox.sandbox.as_ref())
        .await
        .with_context(|| format!("Failed to start {} sandbox", record.provider))?;
    Ok(sandbox)
}

/// Brings a sandbox back into use, idempotently: a running sandbox is left
/// alone; any other is started.
pub(crate) async fn activate(sandbox: &dyn Sandbox) -> Result<(), DriverError> {
    if sandbox.status().await?.state == SandboxState::Running {
        return Ok(());
    }
    sandbox.start().await
}

/// Stop the sandboxes an exited worker can no longer release. This is a
/// supervisor backstop, not a deletion: the workspace and Petri's lease
/// records remain available for inspection, recovery and later pruning.
/// Local process cleanup belongs to the Host provider's worker guardian.
pub(crate) async fn stop_run_sandboxes(
    access: &ProviderAccess,
    environment: &RunEnvironmentSettings,
    run_id: RunId,
) -> anyhow::Result<()> {
    if !environment.lifecycle.stop_on_terminal
        || environment.provider.bundled() == Some(BundledProvider::Local)
    {
        return Ok(());
    }
    let entry = access.entry_for(&environment.provider)?;
    stop_run_sandboxes_on(&access.open()?, &entry, run_id).await
}

async fn stop_run_sandboxes_on(
    sandboxes: &Sandboxes,
    entry: &ProviderName,
    run_id: RunId,
) -> anyhow::Result<()> {
    // Listing also finds a sandbox whose create completed just before the
    // worker died without recording its id, and every nested scope. Only
    // sandboxes carrying the run's label are listed.
    let owned = sandboxes
        .provider(entry.as_str())?
        .list(&LabelFilter::eq(PETRI_RUN_LABEL, run_id.to_string()))
        .await
        .context("Failed to list the exited worker's sandboxes")?;
    let results = join_all(owned.into_iter().map(|summary| async move {
        let result = match sandboxes.attach(&summary.reference).await {
            Ok(sandbox) => sandbox.stop().await,
            Err(error) => Err(error),
        };
        match result {
            // A concurrent run deletion already cleaned it up.
            Ok(()) | Err(DriverError::NotFound { .. }) => Ok(()),
            Err(error) => {
                Err(anyhow::Error::new(error).context("Failed to stop an exited worker's sandbox"))
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

/// Whether the backend behind `kind` is reachable and the configured
/// credential accepted, for preflight. A connection failure is the
/// error; an unhealthy provider is an `Ok` report that says why.
pub(crate) async fn provider_health(
    kind: &SandboxProviderKind,
    access: &ProviderAccess,
) -> anyhow::Result<Health> {
    let entry = access
        .entry_for(kind)
        .with_context(|| format!("Failed to connect to the {kind} provider"))?;
    access
        .open()?
        .provider(entry.as_str())?
        .health()
        .await
        .with_context(|| format!("{kind} health check failed"))
}

/// Whether the Docker daemon answers, for `fabro doctor`.
pub(crate) async fn check_docker_daemon() -> anyhow::Result<()> {
    let health = provider_health(&SandboxProviderKind::DOCKER, &ProviderAccess::default()).await?;
    if health.healthy {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "{}",
        health
            .message
            .unwrap_or_else(|| "Failed to reach Docker daemon".to_string())
    ))
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

/// Whether `credentials` reach Daytona and are accepted. Reachability and
/// authentication failures are errors.
///
/// TODO(parity): sandbox-driver's provider health reports only whether the
/// provider is healthy, with a message. Its Daytona provider works out the
/// scopes a key lacks but the new `Health` drops that list, so a key
/// missing scopes is a rejected credential whose message names them, and
/// Fabro cannot offer its own remediation.
pub(crate) async fn check_daytona_api_key(
    credentials: &DaytonaCredentials,
    probe_timeout: Duration,
) -> anyhow::Result<DaytonaKeyCheck> {
    let access = ProviderAccess {
        providers: ServerSandboxProvidersSettings::default(),
        sandbox:   SandboxProviderConfig::from_lookup(Some(credentials.clone()), |_| None),
    };
    let probe = async {
        let health = provider_health(&SandboxProviderKind::DAYTONA, &access).await?;
        if health.healthy {
            return Ok(DaytonaKeyCheck {
                missing:  Vec::new(),
                required: Vec::new(),
            });
        }
        Err(anyhow::anyhow!(
            "Daytona did not accept the credential: {}",
            health
                .message
                .unwrap_or_else(|| "the control plane gave no reason".to_string())
        ))
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
/// Every entry is opened on first use: the inventory is assembled
/// synchronously at startup, and a provider that is down surfaces as a
/// lookup error rather than a startup failure. A listing keeps only the
/// sandboxes that carry Petri's run label, and a lookup by id answers only
/// for one that does. The `local` kind has an entry too, so a caller can
/// ask whether the kind is ready, but its sandboxes are directories the run
/// record names and there is nothing to list.
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
    Connected(Sandboxes, ProviderName),
    /// The server's catalog, opened through `access` on first use.
    Lazy(Box<LazyConnection>),
}

struct LazyConnection {
    access:  ProviderAccess,
    catalog: OnceCell<(Sandboxes, ProviderName)>,
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

    /// A catalog already open, whose `entry` holds the sandboxes Fabro
    /// persists as `kind`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_connected(
        self,
        kind: SandboxProviderKind,
        sandboxes: Sandboxes,
        entry: &str,
    ) -> Self {
        let entry = ProviderName::try_new(entry).expect("a valid test entry name");
        self.with_entry(kind, Connection::Connected(sandboxes, entry))
    }

    /// A provider reached through `access` on first use.
    #[must_use]
    pub(crate) fn with_lazy(self, kind: SandboxProviderKind, access: ProviderAccess) -> Self {
        self.with_entry(
            kind,
            Connection::Lazy(Box::new(LazyConnection {
                access,
                catalog: OnceCell::new(),
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
    /// The catalog and this kind's entry on it, opened on first use; `None`
    /// when the kind has nothing to list.
    async fn catalog(&self) -> anyhow::Result<Option<(&Sandboxes, &ProviderName)>> {
        match &self.connection {
            Connection::HostDirectories => Ok(None),
            #[cfg(test)]
            Connection::Connected(sandboxes, entry) => Ok(Some((sandboxes, entry))),
            Connection::Lazy(lazy) => {
                let (sandboxes, entry) = lazy
                    .catalog
                    .get_or_try_init(|| async {
                        let entry = lazy.access.entry_for(&self.kind)?;
                        anyhow::Ok((lazy.access.open()?, entry))
                    })
                    .await
                    .with_context(|| format!("Failed to connect to the {} provider", self.kind))?;
                Ok(Some((sandboxes, entry)))
            }
        }
    }

    async fn list(&self) -> anyhow::Result<Vec<SandboxInfo>> {
        let Some((sandboxes, entry)) = self.catalog().await? else {
            return Ok(Vec::new());
        };
        let failed = || format!("Failed to list {} sandboxes", self.kind);
        let provider = sandboxes.provider(entry.as_str()).with_context(failed)?;
        let isolation = provider.describe().await.with_context(failed)?.isolation;
        // The driver filters on a label's value; Petri's run label is a
        // different run id on every sandbox, so the listing is narrowed to
        // the key here.
        let summaries = provider
            .list(&LabelFilter::default())
            .await
            .with_context(failed)?;
        Ok(summaries
            .into_iter()
            .filter(|summary| is_petri_sandbox(&summary.labels))
            .map(|summary| {
                let mut status =
                    SandboxStatus::new(summary.reference.id.as_str(), summary.state.into());
                status.name = summary.name;
                status.labels = summary.labels;
                status.sandbox_kind = fabro_types::SandboxKind::from_isolation(isolation);
                SandboxInfo {
                    provider: self.kind.clone(),
                    status,
                }
            })
            .collect())
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<SandboxInfo>> {
        let Some((sandboxes, entry)) = self.catalog().await? else {
            return Ok(None);
        };
        // An id the driver cannot even name is not one of ours.
        if SandboxId::try_new(id).is_err() {
            return Ok(None);
        }
        let failed = || format!("Failed to look up {} sandbox '{id}'", self.kind);
        let descriptor = describe(sandboxes, entry, &self.kind)
            .await
            .with_context(failed)?;
        let reference = reference_in(&descriptor, entry, &self.kind, id)?;
        let handle = match sandboxes.attach(&reference).await {
            Ok(handle) => handle,
            Err(DriverError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(anyhow::Error::new(error).context(failed())),
        };
        let status = handle
            .status()
            .await
            .with_context(|| format!("Failed to describe {} sandbox '{id}'", self.kind))?;
        // Deleted, or not a run's: neither is in the inventory.
        if status.state == SandboxState::Deleted || !is_petri_sandbox(&status.labels) {
            return Ok(None);
        }
        Ok(Some(SandboxInfo {
            provider: self.kind.clone(),
            status:   SandboxStatus::from_driver(&status, Some(descriptor.isolation)),
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
    //! Fake catalogs holding Petri-labelled sandboxes, for the inventory and
    //! the attach path.

    use sandbox_driver::Sandboxes;
    use sandbox_driver_testing::FakeProvider;

    use super::PETRI_RUN_LABEL;

    /// The fake entry every catalog here holds its sandboxes on.
    pub(crate) const FAKE_ENTRY: &str = "fake";

    /// A fake catalog whose [`FAKE_ENTRY`] holds a running sandbox Petri
    /// created for each of `run_ids`, and those sandboxes' ids, in order.
    pub(crate) async fn petri_fake_catalog(run_ids: &[&str]) -> (Sandboxes, Vec<String>) {
        let sandboxes = FakeProvider::new().sandboxes();
        let mut ids = Vec::new();
        for run_id in run_ids {
            let sandbox = sandboxes
                .builder("default")
                .await
                .expect("the fake environment exists")
                .label(PETRI_RUN_LABEL, run_id)
                .create()
                .await
                .expect("the fake creates a sandbox");
            ids.push(sandbox.reference().id.to_string());
        }
        (sandboxes, ids)
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::RunSandboxRuntime;

    use super::test_support::{FAKE_ENTRY, petri_fake_catalog};
    use super::*;

    fn kind(name: &str) -> SandboxProviderKind {
        SandboxProviderKind::try_new(name).expect("valid kind")
    }

    fn fake_entry() -> ProviderName {
        ProviderName::try_new(FAKE_ENTRY).unwrap()
    }

    fn record(
        provider: SandboxProviderKind,
        id: &str,
        working_directory: &str,
    ) -> RunSandboxInstance {
        RunSandboxInstance {
            provider,
            image: None,
            snapshot: None,
            runtime: RunSandboxRuntime {
                id:                id.to_string(),
                working_directory: working_directory.to_string(),
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
            sandbox: SandboxProviderConfig::default(),
        }
    }

    async fn state_of(sandboxes: &Sandboxes, id: &str) -> SandboxState {
        let descriptor = describe(sandboxes, &fake_entry(), &kind("fake"))
            .await
            .unwrap();
        let reference = reference_in(&descriptor, &fake_entry(), &kind("fake"), id).unwrap();
        sandboxes
            .attach(&reference)
            .await
            .unwrap()
            .status()
            .await
            .unwrap()
            .state
    }

    #[test]
    fn daytona_requires_explicit_credentials() {
        let error = ProviderAccess::default()
            .entry_for(&SandboxProviderKind::DAYTONA)
            .expect_err("daytona must not fall back to the environment");
        assert!(matches!(error, ConnectError::MissingDaytonaCredentials));
    }

    #[test]
    fn disabled_and_unconfigured_kinds_are_refused_before_any_connection() {
        let mut providers = ServerSandboxProvidersSettings::default();
        providers
            .entries
            .insert(SandboxProviderKind::DOCKER, ServerSandboxProviderSettings {
                enabled: false,
                plugin:  None,
            });
        let access = ProviderAccess {
            providers,
            sandbox: SandboxProviderConfig::default(),
        };
        let error = access
            .entry_for(&SandboxProviderKind::DOCKER)
            .expect_err("a disabled provider must not connect");
        assert!(
            matches!(error, ConnectError::Disabled { kind } if kind == SandboxProviderKind::DOCKER)
        );

        let error = ProviderAccess::default()
            .entry_for(&kind("e2b"))
            .expect_err("a plugin kind without settings cannot launch");
        assert!(matches!(error, ConnectError::Unconfigured { kind: k } if k == kind("e2b")));
    }

    #[test]
    fn plugin_settings_become_a_protocol_3_entry() {
        let digest = "a".repeat(64);
        let entry = plugin_entry(
            ProviderKind::try_new("e2b").unwrap(),
            &SandboxPluginSettings {
                path:        Some("/opt/plugins/sandbox-driver-e2b".to_string()),
                sha256:      Some(digest.clone()),
                dev:         true,
                args:        vec!["--verbose".to_string()],
                env:         BTreeMap::from([("REGION".to_string(), "eu".to_string())]),
                inherit_env: vec!["HOME".to_string()],
            },
        )
        .unwrap();
        assert_eq!(entry.plugin.as_str(), "e2b");
        assert_eq!(
            entry.path.as_deref(),
            Some("/opt/plugins/sandbox-driver-e2b")
        );
        // A pinned checksum wins over the development opt-in.
        assert!(matches!(
            entry.trust,
            Some(PluginTrust::Sha256 { digest: ref pinned }) if pinned.as_str() == digest
        ));
        assert_eq!(entry.args, ["--verbose"]);
        assert_eq!(entry.env.get("REGION").map(String::as_str), Some("eu"));
        assert_eq!(entry.inherit_env, Some(vec!["HOME".to_string()]));

        let unpinned = plugin_entry(
            ProviderKind::try_new("nowhere-xyz").unwrap(),
            &SandboxPluginSettings {
                dev: true,
                ..SandboxPluginSettings::default()
            },
        )
        .unwrap();
        assert!(matches!(unpinned.trust, Some(PluginTrust::Development)));
        assert_eq!(unpinned.path, None, "no sandbox-driver-nowhere-xyz on PATH");
    }

    #[tokio::test]
    async fn worker_cleanup_stops_all_owned_sandboxes_without_deleting_them() {
        let run_id = RunId::new();
        let other_run = RunId::new().to_string();
        let (sandboxes, ids) =
            petri_fake_catalog(&[&run_id.to_string(), &run_id.to_string(), &other_run]).await;
        let unlabelled = sandboxes
            .builder("default")
            .await
            .unwrap()
            .create()
            .await
            .unwrap()
            .reference()
            .id
            .to_string();

        stop_run_sandboxes_on(&sandboxes, &fake_entry(), run_id)
            .await
            .unwrap();

        assert_eq!(state_of(&sandboxes, &ids[0]).await, SandboxState::Stopped);
        assert_eq!(state_of(&sandboxes, &ids[1]).await, SandboxState::Stopped);
        assert_eq!(state_of(&sandboxes, &ids[2]).await, SandboxState::Running);
        assert_eq!(
            state_of(&sandboxes, &unlabelled).await,
            SandboxState::Running
        );
    }

    #[tokio::test]
    async fn worker_cleanup_skips_keep_running_and_host_environments() {
        // This plugin cannot connect: skipping the policy must happen first.
        let provider = kind("unreachable-cleanup");
        let access = unreachable_plugin_access(provider.as_str());
        let mut environment = RunEnvironmentSettings {
            provider,
            ..RunEnvironmentSettings::default()
        };
        environment.lifecycle.stop_on_terminal = false;
        stop_run_sandboxes(&access, &environment, RunId::new())
            .await
            .unwrap();
        stop_run_sandboxes(&access, &RunEnvironmentSettings::default(), RunId::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn attach_keys_ownership_on_petris_run_label() {
        let run_id = RunId::new();
        let (sandboxes, ids) =
            petri_fake_catalog(&[&run_id.to_string(), &RunId::new().to_string()]).await;
        let unlabelled = sandboxes
            .builder("default")
            .await
            .unwrap()
            .create()
            .await
            .unwrap()
            .reference()
            .id
            .to_string();

        let mine = attach_owned(
            &sandboxes,
            &fake_entry(),
            &record(kind("fake"), &ids[0], "/"),
            run_id,
        )
        .await
        .expect("the run's own sandbox attaches");
        assert_eq!(mine.sandbox.reference().id.as_str(), ids[0]);
        for foreign in [&ids[1], &unlabelled] {
            let error = attach_owned(
                &sandboxes,
                &fake_entry(),
                &record(kind("fake"), foreign, "/"),
                run_id,
            )
            .await
            .err()
            .expect("a sandbox without the run's label is refused");
            assert!(
                error.to_string().contains("does not belong to run"),
                "{foreign}: {error:#}"
            );
        }
    }

    #[tokio::test]
    async fn a_host_record_reconnects_by_designating_its_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = workspace.path().display().to_string();
        let sandbox = attach_running_run_sandbox(
            &ProviderAccess::default(),
            &record(SandboxProviderKind::LOCAL, "host-recorded-id", &directory),
            RunId::new(),
        )
        .await
        .expect("the recorded directory is designated again");
        assert_eq!(
            sandbox.sandbox.working_directory().canonicalize().unwrap(),
            workspace.path().canonicalize().unwrap()
        );
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
        let (docker, docker_ids) = petri_fake_catalog(&["run-1"]).await;
        let foreign = docker
            .builder("default")
            .await
            .unwrap()
            .create()
            .await
            .unwrap()
            .reference()
            .id
            .to_string();
        // The fakes number their sandboxes alike, so Daytona destroys the
        // sandbox whose id is Docker's unlabelled one; its third sandbox's id
        // is then Daytona's alone.
        let (daytona, mut daytona_ids) = petri_fake_catalog(&["run-2", "run-5", "run-3"]).await;
        assert_eq!(daytona_ids[1], foreign);
        let gone = daytona_ids.remove(1);
        let descriptor = describe(&daytona, &fake_entry(), &kind("fake"))
            .await
            .unwrap();
        let gone = reference_in(&descriptor, &fake_entry(), &kind("fake"), &gone).unwrap();
        daytona.destroy(&gone).await.unwrap();
        let inventory = SandboxInventory::empty()
            .with_host_directories(SandboxProviderKind::LOCAL)
            .with_connected(SandboxProviderKind::DOCKER, docker, FAKE_ENTRY)
            .with_connected(SandboxProviderKind::DAYTONA, daytona, FAKE_ENTRY);

        let response = inventory.list_managed().await;

        let mut listed: Vec<_> = response
            .data
            .iter()
            .map(|s| (s.provider.clone(), s.status.id.clone()))
            .collect();
        listed.sort_unstable();
        let mut expected: Vec<_> = docker_ids
            .iter()
            .map(|id| (SandboxProviderKind::DOCKER, id.clone()))
            .collect();
        expected.extend(
            daytona_ids
                .iter()
                .map(|id| (SandboxProviderKind::DAYTONA, id.clone())),
        );
        expected.sort_unstable();
        assert_eq!(listed, expected);
        assert!(response.meta.provider_errors.is_empty());
        let kinds: Vec<_> = inventory.kinds().cloned().collect();
        assert_eq!(kinds, [
            SandboxProviderKind::LOCAL,
            SandboxProviderKind::DOCKER,
            SandboxProviderKind::DAYTONA
        ]);

        let found = inventory
            .get_managed_by_native_id(&daytona_ids[1])
            .await
            .expect("one provider matches");
        assert_eq!(found.provider, SandboxProviderKind::DAYTONA);
        assert_eq!(
            found.status.labels.get(PETRI_RUN_LABEL).map(String::as_str),
            Some("run-3")
        );
        let error = inventory
            .get_managed_by_native_id(&foreign)
            .await
            .expect_err("a sandbox without Petri's label is not in the inventory");
        assert!(matches!(error, SandboxLookupError::NotFound { .. }));
    }

    #[tokio::test]
    async fn the_inventory_reports_a_provider_that_cannot_connect_beside_the_others() {
        let (docker, _) = petri_fake_catalog(&["run-1"]).await;
        let inventory = SandboxInventory::empty()
            .with_connected(SandboxProviderKind::DOCKER, docker, FAKE_ENTRY)
            .with_lazy(kind("e2b"), unreachable_plugin_access("e2b"));

        let response = inventory.list_managed().await;
        assert_eq!(response.data.len(), 1);
        assert_eq!(response.meta.provider_errors.len(), 1);
        assert_eq!(response.meta.provider_errors[0].provider, kind("e2b"));
        assert!(
            response.meta.provider_errors[0]
                .message
                .contains("/nonexistent/sandbox-driver-e2b"),
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
        let (docker, docker_ids) = petri_fake_catalog(&["run-1"]).await;
        let (daytona, daytona_ids) = petri_fake_catalog(&["run-1"]).await;
        assert_eq!(
            docker_ids, daytona_ids,
            "the fakes number their sandboxes alike"
        );
        let inventory = SandboxInventory::empty()
            .with_connected(SandboxProviderKind::DOCKER, docker, FAKE_ENTRY)
            .with_connected(SandboxProviderKind::DAYTONA, daytona, FAKE_ENTRY);

        let error = inventory
            .get_managed_by_native_id(&docker_ids[0])
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
