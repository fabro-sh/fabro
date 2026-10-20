//! A run's sandboxes deleted through Petri's lease ledger.
//!
//! Petri records every sandbox a run creates as a lease: the provider, the
//! provider's id for the resource, and the fingerprint of the backend it
//! lives on. When Fabro deletes a run, its sandboxes go the way `petri
//! sandbox prune` deletes them, over the store the run's records live in,
//! rather than through a provider call of Fabro's own: Petri opens the run
//! for writing, so a live worker that still holds the lease refuses the
//! delete; it checks each lease's fingerprint against the provider it
//! connects, so a changed daemon or account is a problem to report, never
//! a delete on another backend; it writes the delete intent before the
//! provider call and the tombstone after, beside the run's other records;
//! and each provider removes its sandbox's managed workspace, a host
//! workspace under the run directory included.
//!
//! The runtime a prune runs on is the run's as [`engine`]
//! assembles it, reduced to what a prune reads: the store, the run key, the
//! run directory (where Petri's host registry and action-host markers are)
//! and the sandbox backend. No step registry, frontend or model client
//! takes part.

use std::path::PathBuf;
use std::sync::Arc;

use fabro_types::SandboxProviderKind;
pub use petri_execution::prune::PruneReport;
use petri_execution::prune::{self as petri_prune};
use petri_execution::{RunKey, RunStore};
use petri_runtime::RunOptions;

use crate::engine;
use crate::providers::{self, SandboxProviderConfig};

/// One run whose sandboxes are to be deleted.
pub struct PruneRequest {
    /// The provider configuration used by this server-side operation.
    pub sandbox:  SandboxProviderConfig,
    /// The Fabro run id, which is Petri's run key.
    pub run_id:   String,
    /// Where the run's worker ran Petri: its host registry and action-host
    /// markers are under it, and so is a host workspace.
    pub run_dir:  PathBuf,
    /// The run's durable record.
    pub store:    Arc<dyn RunStore>,
    /// The sandbox provider Fabro resolved for the run's environment.
    pub provider: SandboxProviderKind,
}

/// Why a run's sandboxes could not be pruned.
#[derive(Debug, thiserror::Error)]
pub enum PruneError {
    #[error("the run's sandbox provider `{provider}` is not one Petri serves")]
    UnsupportedProvider { provider: SandboxProviderKind },
    /// The run's provider is a third-party kind the server cannot register.
    #[error("{message}")]
    UnregisteredProvider { message: String },
    /// A live process holds the run's lease: pruning under it would delete
    /// the sandboxes it is using.
    #[error("run {locator} is held by a live process; stop it first")]
    RunHeld { locator: String },
    #[error("the run's sandboxes could not be pruned")]
    Petri(#[source] petri_prune::PruneError),
}

/// Delete every sandbox the run still holds, through Petri's lease ledger.
/// The report says what was deleted, what needed nothing, and which leases
/// could not be pruned and why; their records keep the pending intent, so
/// the next prune tries again.
pub async fn prune(request: PruneRequest) -> Result<PruneReport, PruneError> {
    if let Some(message) = request.sandbox.registration_problem(&request.provider) {
        return Err(PruneError::UnregisteredProvider { message });
    }
    let backend =
        engine::backend(&request.provider).ok_or_else(|| PruneError::UnsupportedProvider {
            provider: request.provider.clone(),
        })?;
    let mut options = RunOptions::new(&request.run_dir);
    options.run_key = Some(RunKey::new(request.run_id.as_str()));
    options.retention = engine::RETENTION;
    options.sandbox.backend = backend;
    let runtime = providers::bare_runtime(&request.sandbox)
        .store(request.store)
        .options(options);
    petri_prune::prune(&runtime)
        .await
        .map_err(|error| match error {
            petri_prune::PruneError::RunHeld(locator) => PruneError::RunHeld { locator },
            other => PruneError::Petri(other),
        })
}
