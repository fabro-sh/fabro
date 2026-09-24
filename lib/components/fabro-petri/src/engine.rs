//! A Fabro run executed by Petri: the one assembly the run's worker process
//! and the server share.
//!
//! The worker is where a Petri run executes, as a legacy run does: it
//! reaches the run's record through [`HttpRunStore`](crate::HttpRunStore)
//! with its token, and everything else here is the same as in the server.
//! The server itself executes a run only under its test override, over
//! [`SqliteRunStore`](crate::SqliteRunStore) in its own process. Both build
//! the runtime the same way the create handler built it for
//! `Runtime::check`, name the Fabro run id as the run key, and hand the run
//! to `execution::host`: [`Execution::Start`] runs the admitted graphs
//! through `run_configured`; [`Execution::Resume`] continues the run from
//! its records through `resume_configured`, with the same observers a start
//! installs, as the host's docs require. The outcome is then derived from
//! `inspect_run` over a read handle of the same store, so what the caller
//! reports is what the durable record says.
//!
//! What the caller supplies beyond the runtime: the interviewer its
//! questions go to ([`interview`](crate::interview) in the worker and the
//! server), the secret provider over the vault ([`secrets`](crate::secrets))
//! and the blob table ([`blobs`](crate::blobs)) when it has them, and a
//! [`HooksSpec`] for Fabro's own [`FabroHooks`], which wrap Petri's local
//! hook service: the checkpoint commit before every durable finish and its
//! platform record after every route, with a failed commit ending the run
//! as a `checkpoint_failed` failure. What the standalone runner's defaults
//! give the run: Petri's local hook service for `[[run.hooks]]`, no host
//! tools, and `Retention::Always` for every workspace (see [`RETENTION`]).
//! Cancellation rides the caller's token: when it fires, the root
//! invocation is cancelled politely and Petri records why. The run's other
//! controls (pause, unpause, steer) are the caller's [`RunControls`]: its
//! pause gate is installed over the run's hooks, it observes the run, and
//! it is wired to the coordinator with the interviewer, on a start and on
//! a resume alike, so a run that was paused resumes paused.
//!
//! A resume here is Petri's own: the run continues from its records, and
//! sandbox leases are reconciled by label. What a host workspace looks like
//! when it does is the server's business before it relaunches the worker
//! ([`recovery`](crate::recovery)); a Docker or Daytona workspace is brought
//! to its snapshot by Fabro's hooks when its scope is acquired, and a lease
//! whose sandbox is gone gets a fresh one to restore into.
//!
//! No stage or agent event is projected into Fabro's tables here; the
//! caller appends only the run lifecycle events Fabro's read side needs to
//! finish the run, from the [`Conclusion`] this module derives. The
//! projection over Petri's records is the read-side item that follows.

use std::path::PathBuf;
use std::sync::Arc;

use fabro_types::{FailureReason, RunId, SandboxProviderKind};
use petri_execution::host::{self, HostError, HostRun};
use petri_execution::inspect::{self, InspectError, RunInspection};
use petri_execution::{
    Access, CancelReason, ExecutionObserver, InterviewDispatcher, Interviewer, InvocationId,
    RECEIPT_FILE, RunKey, RunStore,
};
use petri_runtime::driver::lifecycle::ExecutionHooks;
pub use petri_runtime::executor::Retention;
use petri_runtime::executor::SecretProvider;
use petri_runtime::{LostSandbox, RunOptions, SandboxBackend};
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::admission::AdmittedGraphs;
use crate::artifacts::ArtifactWriter;
use crate::blobs::{Blobs, RunBlobs};
use crate::controls::RunControls;
use crate::hooks::{FabroHooks, HooksSpec};
use crate::runtime::RuntimeSpec;
use crate::secrets::SharedSecrets;

/// How the run is entered: fresh, from the admitted graphs, or continued
/// from its records.
pub enum Execution {
    /// Run the admitted graphs from the start; the run must not exist in
    /// the store yet.
    Start(AdmittedGraphs),
    /// Continue the run from its records; the run must exist in the store
    /// with its root invocation declared.
    Resume,
}

/// One run to execute.
pub struct RunRequest {
    /// The Fabro run id, which becomes Petri's run key: the run's identity
    /// in the store and the label on every sandbox of the run.
    pub run_id:          String,
    /// Where the run's workspaces, step output and blobs live.
    pub run_dir:         PathBuf,
    pub execution:       Execution,
    /// The run's durable record: the worker's HTTP store, or the server's
    /// SQLite store under the test override.
    pub store:           Arc<dyn RunStore>,
    pub runtime:         RuntimeSpec,
    /// The sandbox provider Fabro resolved for the run's environment.
    pub provider:        SandboxProviderKind,
    /// Fires to cancel the run.
    pub cancel:          CancellationToken,
    /// The run's pause, unpause and steer controls, which the caller keeps
    /// a clone of to drive them while the run is live.
    pub controls:        RunControls,
    /// Where the run's questions go.
    pub interviewer:     Arc<dyn Interviewer>,
    /// The caller's observers of every record, registered ahead of the
    /// interview dispatcher: the interviewer's own expiry observer among
    /// them.
    pub observers:       Vec<Arc<dyn ExecutionObserver>>,
    /// Where `{{ secrets.NAME }}` references resolve from; `None` leaves
    /// every secret unknown.
    pub secrets:         Option<Arc<dyn SecretProvider>>,
    /// Where offloaded stage values go; `None` keeps Petri's local store
    /// under the run directory.
    pub blobs:           Option<Arc<dyn Blobs>>,
    /// Where captured workspace files go. Required when capture writes files.
    pub artifact_writer: Option<Arc<dyn ArtifactWriter>>,
    /// Fabro's hooks: the checkpoint commit and its record. `None` runs
    /// with Petri's local hook service alone.
    pub hooks:           Option<HooksSpec>,
}

/// The recorded status of a finished run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Success,
    Failed,
    Cancelled,
}

/// What the durable record says about the run once it ended.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub status:     RunStatus,
    /// The root invocation's failure message, when it failed.
    pub failure:    Option<String>,
    /// Whether the record is whole: the run recorded its finish and every
    /// log replays byte for byte.
    pub complete:   bool,
    /// Every reason `complete` is false.
    pub incomplete: Vec<String>,
}

/// Why the run could not be executed or its outcome read.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("the run's sandbox provider `{provider}` is not one Petri serves")]
    UnsupportedProvider { provider: SandboxProviderKind },
    #[error("the run's record could not be opened")]
    Open(#[source] petri_store::StoreError),
    #[error("the run's record could not be read")]
    Read(#[source] HostError),
    #[error("the run's record has no root invocation, so there is nothing to resume")]
    NothingToResume,
    #[error("the run's record could not be inspected")]
    Inspect(#[source] InspectError),
    #[error("the run ended without recording a status; the record says: {}", .0.join("; "))]
    Unfinished(Vec<String>),
}

/// How Fabro reports the run: what its read side records as the run's
/// terminal event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conclusion {
    /// The record says the run succeeded and is whole.
    Succeeded,
    /// Anything else: the record says the run failed or was cancelled, the
    /// record is incomplete, or the run could not be executed at all.
    Failed {
        reason:  FailureReason,
        message: String,
    },
}

/// Execute the run to its end and report what the record says.
pub async fn run(request: RunRequest) -> Result<RunOutcome, RunError> {
    let backend = backend(&request.provider).ok_or_else(|| RunError::UnsupportedProvider {
        provider: request.provider.clone(),
    })?;
    let key = RunKey::new(request.run_id.as_str());
    let mut options = RunOptions::new(&request.run_dir);
    options.run_key = Some(key.clone());
    options.retention = RETENTION;
    options.sandbox.backend = backend;
    // Fabro's hooks restore a sandbox workspace from its snapshots at the
    // scope's acquisition, so a lease whose sandbox is gone gets a fresh
    // one instead of failing the run.
    if request.hooks.is_some() && backend != SandboxBackend::Host {
        options.sandbox.lost_sandbox = LostSandbox::Replace;
    }
    let resumed = matches!(request.execution, Execution::Resume);
    let mut runtime = request
        .runtime
        .runtime(true)
        .store(Arc::clone(&request.store))
        .options(options);
    if let Some(secrets) = request.secrets {
        runtime = runtime.secrets(SharedSecrets(secrets));
    }
    if let Some(blobs) = &request.blobs {
        runtime = runtime.capability(RunBlobs::output_store(Arc::clone(blobs)));
    }
    let fabro_hooks = request.hooks.map(|spec| {
        let inner = runtime
            .installed_hooks()
            .unwrap_or_else(|| Arc::new(NoHooks));
        let run_id = spec_run_id(&request.run_id);
        Arc::new(FabroHooks::new(
            spec,
            inner,
            run_id,
            key.clone(),
            request.run_dir.clone(),
            Arc::clone(&request.store),
            resumed,
            request.blobs.clone(),
            request.artifact_writer.clone(),
        ))
    });
    if let Some(hooks) = &fabro_hooks {
        runtime = runtime.hooks(Arc::clone(hooks) as Arc<dyn ExecutionHooks>);
    }
    // The pause gate goes outermost, over Fabro's hooks and Petri's own,
    // so a held attempt runs none of them until the unpause.
    // The live-turn set beside it: what an interrupt can reach.
    let controls = request.controls;
    let installed = runtime.installed_hooks();
    runtime = runtime
        .hooks(controls.hooks(installed))
        .capability(controls.turns());

    let dispatcher = InterviewDispatcher::new(request.interviewer);
    let cancel = request.cancel.clone();
    let mut cancel_task = None;
    let with_handle = |handle: petri_execution::CoordinatorHandle, secrets| {
        dispatcher.wire(handle.clone(), secrets);
        if let Some(hooks) = &fabro_hooks {
            hooks.attach(handle.clone());
        }
        controls.wire(handle.clone());
        cancel_task = Some(tokio::spawn(async move {
            cancel.cancelled().await;
            info!("cancelling the Petri run");
            handle.cancel_root_for(CancelReason::Control);
        }));
    };
    let mut observers = request.observers;
    observers.push(controls.observer());
    observers.push(Arc::new(dispatcher.clone()));
    let result = match request.execution {
        Execution::Start(graphs) => {
            info!(run_id = %request.run_id, backend = %backend, "Starting Petri run");
            let mut host_run = HostRun::new(graphs.graph).with_children(graphs.children);
            for observer in observers {
                host_run = host_run.observe(observer);
            }
            Box::pin(host::run_configured(&runtime, host_run, with_handle)).await
        }
        Execution::Resume => {
            check_resumable(request.store.as_ref(), &key).await?;
            info!(run_id = %request.run_id, backend = %backend, "Resuming Petri run");
            Box::pin(host::resume_configured(
                &runtime,
                Vec::new(),
                observers,
                with_handle,
            ))
            .await
        }
    };
    if let Some(task) = cancel_task {
        task.abort();
    }
    let receipt = dispatcher.shutdown().await;
    write_receipt(&request.run_dir, &receipt).await;
    match &result {
        Ok(report) => debug!(status = %report.status, "Petri run ended"),
        Err(error) => warn!(error = %error, "Petri run ended with a host error"),
    }
    let inspection = inspect(request.store.as_ref(), &key).await?;
    let mut outcome = outcome(inspection, result.err())?;
    // A failed checkpoint cancelled the run; what Fabro reports is the
    // checkpoint failure, not a cancellation.
    if let Some(failure) = fabro_hooks
        .as_ref()
        .and_then(|hooks| hooks.checkpoint_failure())
    {
        outcome.status = RunStatus::Failed;
        outcome.failure = Some(failure);
    }
    Ok(outcome)
}

/// When Petri keeps a run's workspaces after their scope is released.
///
/// Petri's retention makes one choice at release: keep the workspace (a
/// host directory, a container, a remote sandbox) or remove it. Fabro's
/// environment lifecycle settings make different choices: `stop_on_terminal`
/// says whether a sandbox keeps running after the run, and `preserve` says
/// whether deleting the run may remove it. Neither asks for a sandbox to be
/// removed when the run ends: the legacy executor stopped a container at
/// the end and left it for the sandbox tab, `fabro cp`, the run's delete
/// and `fabro system prune`, and a host workspace lives under the run's
/// scratch directory, which goes with the run. So every setting maps to
/// `Retention::Always`, and `Retention::OnFailure` and `Retention::Never`
/// have no Fabro setting that names them.
pub const RETENTION: Retention = Retention::Always;

/// The Fabro run id the run key names. A key that is not one (a test's
/// bare key) still gets hooks, under a fresh id for its platform records.
fn spec_run_id(run_id: &str) -> RunId {
    run_id.parse().unwrap_or_else(|_| {
        warn!(
            run_id,
            "the Petri run key is not a Fabro run id; platform records use a fresh id"
        );
        RunId::new()
    })
}

/// No host hooks at all: what Fabro's hooks wrap when the runtime installed
/// none.
struct NoHooks;

impl ExecutionHooks for NoHooks {}

/// What the run's record says, read through a handle that holds no lease:
/// the same derivation [`run`] ends with, for a caller that only holds the
/// store, such as a test checking a finished run.
pub async fn outcome_of(store: &dyn RunStore, run_id: &str) -> Result<RunOutcome, RunError> {
    let inspection = inspect(store, &RunKey::new(run_id)).await?;
    outcome(inspection, None)
}

/// How Fabro reports what [`run`] returned. A cancelled run is a failure
/// with the cancelled reason, as the legacy executor reports one; every
/// other shortfall is a workflow error whose message says what the record,
/// or the host, said.
#[must_use]
pub fn conclusion(result: &Result<RunOutcome, RunError>) -> Conclusion {
    match result {
        Ok(RunOutcome {
            status: RunStatus::Success,
            complete: true,
            ..
        }) => Conclusion::Succeeded,
        Ok(outcome) => {
            let reason = match outcome.status {
                RunStatus::Cancelled => FailureReason::Cancelled,
                RunStatus::Success | RunStatus::Failed => FailureReason::WorkflowError,
            };
            Conclusion::Failed {
                reason,
                message: failure_message(outcome),
            }
        }
        Err(error) => Conclusion::Failed {
            reason:  FailureReason::WorkflowError,
            message: error_chain(error),
        },
    }
}

/// The failure of a run whose record says it did not succeed.
fn failure_message(outcome: &RunOutcome) -> String {
    let mut message = match (&outcome.status, &outcome.failure) {
        (RunStatus::Cancelled, _) => "the run was cancelled".to_string(),
        (_, Some(failure)) => failure.clone(),
        (RunStatus::Failed, None) => "the run failed".to_string(),
        (RunStatus::Success, None) => "the run's record is incomplete".to_string(),
    };
    if !outcome.complete {
        message.push_str(" (record incomplete: ");
        message.push_str(&outcome.incomplete.join("; "));
        message.push(')');
    }
    message
}

/// The error and every cause under it, as one line.
fn error_chain(error: &RunError) -> String {
    let mut parts = vec![error.to_string()];
    let mut cause = std::error::Error::source(error);
    while let Some(next) = cause {
        parts.push(next.to_string());
        cause = next.source();
    }
    parts.join(": ")
}

/// The sandbox backend for Fabro's provider kind; `None` for a kind Petri
/// does not serve.
pub(crate) fn backend(provider: &SandboxProviderKind) -> Option<SandboxBackend> {
    if *provider == SandboxProviderKind::LOCAL {
        Some(SandboxBackend::Host)
    } else if *provider == SandboxProviderKind::DOCKER {
        Some(SandboxBackend::Docker)
    } else if *provider == SandboxProviderKind::DAYTONA {
        Some(SandboxBackend::Daytona)
    } else {
        None
    }
}

/// Refuse a resume the host would not survive: `resume_configured` indexes
/// the root invocation of the stored state, so a record with none (the run
/// was created in the store and nothing more) is refused here with a named
/// error instead.
async fn check_resumable(store: &dyn RunStore, key: &RunKey) -> Result<(), RunError> {
    let logs = store
        .open(key, Access::Read)
        .await
        .map_err(RunError::Open)?;
    let state = host::stored_state(&*logs).await.map_err(RunError::Read)?;
    if state.invocations.contains_key(&InvocationId::ROOT) {
        Ok(())
    } else {
        Err(RunError::NothingToResume)
    }
}

/// Read the run back through a handle that holds no lease.
async fn inspect(store: &dyn RunStore, key: &RunKey) -> Result<RunInspection, RunError> {
    let logs = store
        .open(key, Access::Read)
        .await
        .map_err(RunError::Open)?;
    inspect::inspect_run(&*logs)
        .await
        .map_err(RunError::Inspect)
}

/// The outcome the record supports. A run whose record has no status is
/// unfinished: the host error, when there is one, says why.
fn outcome(
    inspection: RunInspection,
    host_error: Option<HostError>,
) -> Result<RunOutcome, RunError> {
    let status = match inspection.status.as_deref() {
        Some("success") => RunStatus::Success,
        Some("failed") => RunStatus::Failed,
        Some("cancelled") => RunStatus::Cancelled,
        _ => {
            let mut reasons = inspection.incomplete.clone();
            if let Some(error) = host_error {
                reasons.push(error.to_string());
            }
            return Err(RunError::Unfinished(reasons));
        }
    };
    let failure = inspection
        .invocations
        .iter()
        .find(|invocation| invocation.invocation == inspection.root.invocation)
        .and_then(|root| root.result.as_ref())
        .and_then(|result| result.failure.as_ref())
        .map(|failure| failure.message.clone());
    Ok(RunOutcome {
        status,
        failure,
        complete: inspection.complete,
        incomplete: inspection.incomplete,
    })
}

/// The interview receipt beside the run, as the standalone runner writes
/// it. A receipt that cannot be written is logged: the run's record does
/// not depend on it.
async fn write_receipt(run_dir: &std::path::Path, receipt: &petri_execution::InterviewReceipt) {
    let path = run_dir.join(RECEIPT_FILE);
    let bytes = match serde_json::to_vec_pretty(receipt) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(error = %error, "could not encode the interview receipt");
            return;
        }
    };
    if let Err(error) = fs::create_dir_all(run_dir).await {
        warn!(path = %run_dir.display(), error = %error, "could not create the run directory");
        return;
    }
    if let Err(error) = fs::write(&path, bytes).await {
        warn!(path = %path.display(), error = %error, "could not write the interview receipt");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome_with(status: RunStatus, failure: Option<&str>, complete: bool) -> RunOutcome {
        RunOutcome {
            status,
            failure: failure.map(ToOwned::to_owned),
            complete,
            incomplete: if complete {
                Vec::new()
            } else {
                vec!["execution 0 did not finish".to_string()]
            },
        }
    }

    #[test]
    fn a_whole_successful_record_concludes_succeeded() {
        assert_eq!(
            conclusion(&Ok(outcome_with(RunStatus::Success, None, true))),
            Conclusion::Succeeded
        );
    }

    #[test]
    fn a_cancelled_record_concludes_cancelled() {
        assert_eq!(
            conclusion(&Ok(outcome_with(RunStatus::Cancelled, None, true))),
            Conclusion::Failed {
                reason:  FailureReason::Cancelled,
                message: "the run was cancelled".to_string(),
            }
        );
    }

    #[test]
    fn a_failed_record_carries_the_root_failure_and_the_incomplete_reasons() {
        assert_eq!(
            conclusion(&Ok(outcome_with(
                RunStatus::Failed,
                Some("step `say` failed"),
                false
            ))),
            Conclusion::Failed {
                reason:  FailureReason::WorkflowError,
                message: "step `say` failed (record incomplete: execution 0 did not finish)"
                    .to_string(),
            }
        );
    }

    #[test]
    fn a_host_error_concludes_with_its_chain() {
        let error = RunError::Unfinished(vec!["no status".to_string()]);
        assert_eq!(conclusion(&Err(error)), Conclusion::Failed {
            reason:  FailureReason::WorkflowError,
            message: "the run ended without recording a status; the record says: no status"
                .to_string(),
        });
    }
}
