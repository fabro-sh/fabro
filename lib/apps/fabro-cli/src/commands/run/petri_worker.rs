//! A Petri run in the worker process.
//!
//! `fabro run __run-worker` executes its run here, over the worker services:
//! the authenticated client, the control channel the server pushes cancels
//! through, the signal handlers, the vault snapshot and the CLI catalog. The
//! engine assembly itself is `fabro_petri::engine`, shared with the server's
//! in-process test path, so the run gets the same runtime, options and
//! interviewer either way.
//!
//! The run's record is [`HttpRunStore`] over the worker's client, leased
//! for this launch: the worker mints one owner id at start, logs it, and
//! every lease the run takes over the API names it. `--mode start` loads
//! the admitted graphs through the client's blob read and runs them;
//! `--mode resume` continues the run from its records. Either way the
//! worker records the lifecycle transitions Fabro's read side needs
//! (`starting`, `running`, then `succeeded` or `failed`) as platform
//! records through the client.
//!
//! The server's controls arrive over the control channel and go to Petri
//! through [`PetriControls`]: cancel (and `SIGTERM`/`SIGINT`) fires one
//! token, which cancels Petri's root invocation politely; an
//! `interview.answer` message reaches the control interviewer the run's
//! questions wait on (`fabro_petri::interview`), so a human gate answered
//! through the API continues; pause and unpause (and `SIGUSR1`/`SIGUSR2`)
//! hold and release admission through the run's [`RunControls`]; a steer
//! goes to the agent stage it names (`node@visit`, or the node name) or,
//! unnamed, to the run's one live agent stage, and is refused with a
//! `run.notice` record saying why when neither resolves; an interrupt
//! resolves its stage the same way and stops the stage's current model
//! turn, with the text of an `interrupt_then_steer` as the stage's next
//! input, and is refused with a `run.notice` (`no_live_turn`,
//! `no_such_stage`) when Petri refuses it. A steer or an interrupt that
//! carries a request id is acknowledged over the control channel with its
//! outcome, delivered or refused with the notice's code, so the server can
//! answer the caller in its own response. The
//! paused state is mirrored to Fabro's lifecycle: a `paused` lifecycle
//! record when admission is held and `unpaused` when it is released, so
//! the server's live status and the projection agree with Petri's own
//! `run.paused` and `run.unpaused` records. A resumed run that was paused when
//! its worker died comes back paused, and the mirror reports that too. The
//! pair controls have no Petri adapter yet and are ignored with a warning.
//! A control channel that is lost for good cancels the run the same
//! way, and the worker exits with that loss as its error once the run has
//! settled.
//!
//! Fabro's hooks ride the run with their platform records over the same
//! client: the checkpoint commit in the run's workspace, on the host or
//! inside its sandbox, before every durable finish, and its record after
//! every route.
//!
//! The runtime's settings layer is left empty here: the run's graphs were
//! lowered and admitted at create time with the server's layer, and nothing
//! lowers again at execution. The model client is built from the worker's
//! catalog and vault snapshot for the providers whose credentials resolve,
//! the same eligible set the legacy worker's LLM backend uses. The same
//! vault snapshot is the run's secret provider, the run's blobs go to the
//! server's blob table through the worker's client, and the Fabro home the
//! server named on the command line is the home the skills step reads.
//! Fabro's run tools go to every agent session of the run when the run's
//! settings enable them (`[run.agent] fabro_tools`) and the worker token
//! carries the `agent:run_tools` scope the server issues for such a run,
//! the same gate the legacy worker applies; they bind to the worker's
//! client and the run id, as the legacy worker binds them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use fabro_auth::VaultCredentialSource;
use fabro_client::{Client, ServerTarget};
use fabro_interview::{ControlInterviewer, WorkerControlMessage, WorkerControlOutcome};
use fabro_llm::credentials::{CredentialProvider, readiness};
use fabro_petri::blobs::ClientBlobs;
use fabro_petri::controls::{RunControls, SteerError};
use fabro_petri::engine::{self, Conclusion, Execution, RunRequest};
use fabro_petri::hooks::HooksSpec;
use fabro_petri::interview::{Approval, FabroInterviewer};
use fabro_petri::petri::OwnerId;
use fabro_petri::platform_records::{HttpPlatformRecords, PlatformRecords};
use fabro_petri::runtime::{self, RuntimeSpec};
use fabro_petri::secrets::VaultSecrets;
use fabro_petri::{HttpRunStore, admission};
use fabro_static::EnvVars;
use fabro_store::RunProjection;
use fabro_store::platform_records::{
    PlatformRecord, RunLifecycleKind, RunLifecycleRecord, RunNoticeRecord,
};
use fabro_types::settings::run::{ApprovalMode, RunMode};
use fabro_types::{FailureReason, Principal, RunId, RunNoticeLevel, RunStatus, SuccessReason};
use fabro_vault::Vault;
use fabro_workflow::Error as WorkflowError;
use fabro_workflow::services::FabroRunToolServices;
use tokio::sync::RwLock as AsyncRwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::runner::{self, WorkerTitlePhase};
use crate::args::RunWorkerMode;
use crate::command_context;

/// What the worker holds when it hands a run to Petri.
pub(super) struct PetriWorker<'a> {
    pub(super) run_id:       RunId,
    pub(super) target:       ServerTarget,
    pub(super) client:       Client,
    pub(super) run_state:    RunProjection,
    pub(super) storage_dir:  &'a Path,
    pub(super) run_dir:      PathBuf,
    pub(super) mode:         RunWorkerMode,
    /// The Fabro home the server named; `None` falls back to Petri's own
    /// lookup of the worker's environment.
    pub(super) fabro_home:   Option<PathBuf>,
    pub(super) worker_token: &'a str,
}

/// Execute the run to its end. `Ok` when the record says it succeeded;
/// the failure otherwise, after the terminal event is appended, so the
/// worker exits as the legacy worker does for a failed run.
pub(super) async fn execute(worker: PetriWorker<'_>) -> Result<()> {
    let run_id = worker.run_id;
    let admission = worker.run_state.spec.admission.clone();
    let owner = OwnerId::mint();
    info!(
        run_id = %run_id,
        owner = %owner,
        mode = ?worker.mode,
        "Petri worker starting; every lease of this run names this launch"
    );
    let store = Arc::new(HttpRunStore::for_worker(
        worker.client.clone_for_reuse(),
        owner,
    ));

    let cancel_token = CancellationToken::new();
    let controls = RunControls::new();
    runner::install_signal_handlers(cancel_token.clone(), controls.clone())?;
    let interviewer = Arc::new(ControlInterviewer::new());
    // Fabro's own records of the run, over the client.
    let records: Arc<dyn PlatformRecords> =
        Arc::new(HttpPlatformRecords::new(worker.client.clone_for_reuse()));
    let petri_controls = Arc::new(
        PetriControls::new(run_id, controls.clone(), Arc::clone(&records))
            .with_muted_acks(test_control_acks_muted()),
    );
    let mut control_manager = runner::spawn_worker_control_manager(
        worker.target.clone(),
        run_id,
        worker.worker_token.to_owned(),
        Arc::clone(&interviewer),
        cancel_token.clone(),
        petri_controls,
    );
    control_manager.wait_for_first_connection().await?;
    let approval = if worker.run_state.spec.settings.run.execution.approval == ApprovalMode::Auto {
        Approval::Auto
    } else {
        Approval::Prompt
    };
    let petri_interviewer = FabroInterviewer::new(interviewer, approval);
    let observers = vec![petri_interviewer.observer()];

    let vault = runner::load_worker_vault(worker.storage_dir).await?;
    let secrets = VaultSecrets::from_vault(&*vault.read().await);
    let run_tools = run_tool_services(&worker);
    let runtime = runtime_spec(
        &vault,
        &worker.run_state,
        worker.fabro_home.clone(),
        run_tools,
    )
    .await?;
    let execution = match worker.mode {
        RunWorkerMode::Start => {
            let client = worker.client.clone_for_reuse();
            let graphs = admission::load_with(
                |blob| {
                    let client = client.clone_for_reuse();
                    async move { client.read_run_blob(&run_id, &blob).await }
                },
                &admission,
            )
            .await
            .context("loading the admitted graphs")?;
            Execution::Start(graphs)
        }
        RunWorkerMode::Resume => Execution::Resume,
    };

    let started = Instant::now();
    for transition in [
        (RunLifecycleKind::Starting, RunStatus::Starting),
        (RunLifecycleKind::Running, RunStatus::Running),
    ] {
        lifecycle(&records, run_id, transition.0, transition.1, None).await?;
    }
    runner::set_worker_title(&run_id, WorkerTitlePhase::Running);

    let hooks = HooksSpec::for_run(Arc::clone(&records), &worker.run_state.spec.settings.run)
        .with_test_gates(test_checkpoint_gates());
    let request = RunRequest {
        run_id: run_id.to_string(),
        run_dir: worker.run_dir.join("petri"),
        execution,
        store,
        runtime,
        provider: worker
            .run_state
            .spec
            .settings
            .run
            .environment
            .provider
            .clone(),
        cancel: cancel_token.clone(),
        controls: controls.clone(),
        interviewer: Arc::new(petri_interviewer),
        observers,
        secrets: Some(Arc::new(secrets)),
        blobs: Some(Arc::new(ClientBlobs::new(
            worker.client.clone_for_reuse(),
            run_id,
        ))),
        hooks: Some(hooks),
    };
    let paused_mirror = mirror_paused_state(run_id, &controls, Arc::clone(&records));
    let run = Box::pin(engine::run(request));
    tokio::pin!(run);
    let mut control_lost = None;
    let result = loop {
        tokio::select! {
            result = &mut run => break result,
            lost = control_manager.fatal_control_loss(), if control_lost.is_none() => {
                // The server can no longer reach this worker: end the run
                // politely, let Petri record why, then report the loss.
                warn!(run_id = %run_id, error = %lost, "worker control lost; cancelling the Petri run");
                control_lost = Some(lost);
                cancel_token.cancel();
            }
        }
    };
    control_manager.finish();
    paused_mirror.abort();

    info!(
        run_id = %run_id,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "Petri run ended"
    );
    let (record, phase, failure) = match engine::conclusion(&result) {
        Conclusion::Succeeded => {
            info!(run_id = %run_id, "Petri run completed");
            (
                (
                    RunLifecycleKind::Succeeded,
                    RunStatus::Succeeded {
                        reason: SuccessReason::Completed,
                    },
                    None,
                ),
                WorkerTitlePhase::Succeeded,
                None,
            )
        }
        Conclusion::Failed { reason, message } => {
            info!(run_id = %run_id, error = %message, "Petri run did not succeed");
            let detail = match reason {
                FailureReason::Cancelled => WorkflowError::Cancelled.to_string(),
                _ => message.clone(),
            };
            let phase = if reason == FailureReason::Cancelled {
                WorkerTitlePhase::Cancelled
            } else {
                WorkerTitlePhase::Failed
            };
            // A cancelled run ended the way it was asked to: the worker
            // exits cleanly; any other failure is the worker's exit status.
            let failure = (reason != FailureReason::Cancelled).then_some(message);
            (
                (
                    RunLifecycleKind::Failed,
                    RunStatus::Failed { reason },
                    Some(detail),
                ),
                phase,
                failure,
            )
        }
    };
    lifecycle(&records, run_id, record.0, record.1, record.2).await?;
    runner::set_worker_title(&run_id, phase);
    if let Some(lost) = control_lost {
        return Err(lost);
    }
    match failure {
        None => Ok(()),
        Some(message) => Err(anyhow!("Petri run failed: {message}")),
    }
}

/// The Petri run's controls as the worker's control channel drives them.
/// Cancel and answers are applied by the channel itself, before a message
/// reaches here.
pub(super) struct PetriControls {
    run_id:     RunId,
    controls:   RunControls,
    /// Where a refused steer's notice goes.
    records:    Arc<dyn PlatformRecords>,
    /// A test hook: the worker applies every control but acknowledges
    /// none, so the server's wait for an answer runs out.
    muted_acks: bool,
}

impl PetriControls {
    pub(super) fn new(
        run_id: RunId,
        controls: RunControls,
        records: Arc<dyn PlatformRecords>,
    ) -> Self {
        Self {
            run_id,
            controls,
            records,
            muted_acks: false,
        }
    }

    /// Apply every control but acknowledge none: a test's stand-in for a
    /// worker that never answers.
    #[must_use]
    pub(super) fn with_muted_acks(mut self, muted: bool) -> Self {
        self.muted_acks = muted;
        self
    }

    /// The run's controls, for a test that reads the paused state back.
    #[cfg(test)]
    pub(super) fn controls(&self) -> &RunControls {
        &self.controls
    }

    /// Apply the control. The outcome, for the channel to acknowledge when
    /// the control asked for one: `None` for a control that has no
    /// outcome to report (a pause, an ignored pair control) and when the
    /// acknowledgements are muted.
    pub(super) async fn apply(
        &self,
        message: WorkerControlMessage,
    ) -> Option<WorkerControlOutcome> {
        let outcome = match message {
            WorkerControlMessage::RunPause => {
                info!(run_id = %self.run_id, "pause requested: admission is held");
                self.controls.pause();
                None
            }
            WorkerControlMessage::RunUnpause => {
                self.controls.unpause().await;
                info!(run_id = %self.run_id, "unpause recorded: admission is released");
                None
            }
            WorkerControlMessage::Steer {
                text, stage, actor, ..
            } => Some(self.steer(stage.as_deref(), &text, &actor).await),
            WorkerControlMessage::Interrupt { stage, actor, .. } => {
                Some(self.interrupt(stage.as_deref(), None, &actor).await)
            }
            WorkerControlMessage::InterruptThenSteer {
                text, stage, actor, ..
            } => Some(self.interrupt(stage.as_deref(), Some(&text), &actor).await),
            WorkerControlMessage::PairStart { .. }
            | WorkerControlMessage::PairMessage { .. }
            | WorkerControlMessage::PairEnd { .. } => {
                warn!(
                    run_id = %self.run_id,
                    control = control_name(&message),
                    "control has no Petri adapter yet and is ignored"
                );
                None
            }
            WorkerControlMessage::InterviewAnswer { .. } | WorkerControlMessage::RunCancel => None,
        };
        if self.muted_acks { None } else { outcome }
    }

    /// Deliver `text` to the named stage, or to the run's one live agent
    /// stage. A refusal is a `run.notice` whose code says why
    /// (`no_such_stage` when the name is not running, `steer_refused`
    /// otherwise) and the same code and reason go back as the outcome.
    async fn steer(
        &self,
        stage: Option<&str>,
        text: &str,
        actor: &Principal,
    ) -> WorkerControlOutcome {
        match self.controls.steer(stage, text).await {
            Ok(stage) => {
                info!(run_id = %self.run_id, stage, actor = ?actor, "steer delivered");
                WorkerControlOutcome::Delivered { stage: Some(stage) }
            }
            Err(error) => {
                warn!(run_id = %self.run_id, stage, error = %error, "steer refused");
                self.refuse(
                    error.code().unwrap_or("steer_refused"),
                    refusal_message("Steer", stage, &error),
                )
                .await
            }
        }
    }

    /// Stop the named stage's model turn, `text` as its next input when
    /// given. A refusal is a `run.notice` whose code says why (`no_live_turn`
    /// when the stage has no model turn in flight, `no_such_stage` when the
    /// name is not running, `interrupt_refused` otherwise) and whose message
    /// names the stage and the reason as Petri spells it, for the web and
    /// the CLI to show; the same code and reason go back as the outcome.
    async fn interrupt(
        &self,
        stage: Option<&str>,
        text: Option<&str>,
        actor: &Principal,
    ) -> WorkerControlOutcome {
        match self.controls.interrupt(stage, text).await {
            Ok(stage) => {
                info!(
                    run_id = %self.run_id,
                    stage,
                    steered = text.is_some(),
                    actor = ?actor,
                    "interrupt delivered"
                );
                WorkerControlOutcome::Delivered { stage: Some(stage) }
            }
            Err(error) => {
                warn!(run_id = %self.run_id, stage, error = %error, "interrupt refused");
                self.refuse(
                    error.code().unwrap_or("interrupt_refused"),
                    refusal_message("Interrupt", stage, &error),
                )
                .await
            }
        }
    }

    /// A refused control: its `run.notice` on the run, and the refusal as
    /// the outcome to acknowledge.
    async fn refuse(&self, code: &str, message: String) -> WorkerControlOutcome {
        self.notice(code, message.clone()).await;
        WorkerControlOutcome::Refused {
            code: code.to_string(),
            message,
        }
    }

    /// A `run.notice` record on the run, so a refused control is visible in
    /// the run's stream and not only in the worker's log.
    async fn notice(&self, code: &str, message: String) {
        let record = PlatformRecord::RunNotice(RunNoticeRecord {
            level: RunNoticeLevel::Warn,
            code: code.to_string(),
            message,
        });
        if let Err(error) = self.records.append(&self.run_id, &record, None).await {
            warn!(run_id = %self.run_id, error = %error, "the control notice was not recorded");
        }
    }
}

/// The message of a refused control: the control, the stage it named, and
/// the reason as Petri's `ControlError` (or the resolution's own refusal)
/// spells it.
fn refusal_message(control: &str, stage: Option<&str>, error: &SteerError) -> String {
    match stage {
        Some(stage) => format!("{control} of stage `{stage}` refused: {error}"),
        None => format!("{control} refused: {error}"),
    }
}

/// The wire name of a control, for a log line.
fn control_name(message: &WorkerControlMessage) -> &'static str {
    match message {
        WorkerControlMessage::InterviewAnswer { .. } => "interview.answer",
        WorkerControlMessage::RunCancel => "run.cancel",
        WorkerControlMessage::RunPause => "run.pause",
        WorkerControlMessage::RunUnpause => "run.unpause",
        WorkerControlMessage::Steer { .. } => "run.steer",
        WorkerControlMessage::Interrupt { .. } => "run.interrupt",
        WorkerControlMessage::InterruptThenSteer { .. } => "run.interrupt_then_steer",
        WorkerControlMessage::PairStart { .. } => "pair.start",
        WorkerControlMessage::PairMessage { .. } => "pair.message",
        WorkerControlMessage::PairEnd { .. } => "pair.end",
    }
}

/// One lifecycle transition of the run, recorded through the client.
async fn lifecycle(
    records: &Arc<dyn PlatformRecords>,
    run_id: RunId,
    transition: RunLifecycleKind,
    status: RunStatus,
    reason: Option<String>,
) -> Result<()> {
    let mut record = RunLifecycleRecord::new(transition).with_status(status);
    record.reason = reason;
    records
        .append(&run_id, &PlatformRecord::RunLifecycle(record), None)
        .await
        .with_context(|| format!("recording the run's {transition} transition"))?;
    Ok(())
}

/// Mirror the run's paused state to Fabro's lifecycle: `paused` when
/// admission is held (a pause, or a resume that came back paused) and
/// `unpaused` when it is released, each once per change, with the
/// worker's title alongside. Aborted with the run.
fn mirror_paused_state(
    run_id: RunId,
    controls: &RunControls,
    records: Arc<dyn PlatformRecords>,
) -> JoinHandle<()> {
    let mut changes = controls.paused_changes();
    tokio::spawn(async move {
        let mut last = *changes.borrow_and_update();
        while changes.changed().await.is_ok() {
            let paused = *changes.borrow_and_update();
            if paused == last {
                continue;
            }
            last = paused;
            let (transition, phase) = if paused {
                (RunLifecycleKind::Paused, WorkerTitlePhase::Paused)
            } else {
                (RunLifecycleKind::Unpaused, WorkerTitlePhase::Running)
            };
            let record = PlatformRecord::RunLifecycle(RunLifecycleRecord::new(transition));
            if let Err(error) = records.append(&run_id, &record, None).await {
                warn!(run_id = %run_id, error = %error, "the paused state was not reported");
            }
            runner::set_worker_title(&run_id, phase);
        }
    })
}

/// A test's checkpoint gate directory, when the server forwarded one.
#[expect(
    clippy::disallowed_methods,
    reason = "the gate directory is a test-only process-env facade the server forwards by name"
)]
fn test_checkpoint_gates() -> Option<PathBuf> {
    std::env::var_os(EnvVars::FABRO_TEST_CHECKPOINT_GATES).map(PathBuf::from)
}

/// Whether a test asked this worker to acknowledge no control, so the
/// server's wait for an answer runs out.
#[expect(
    clippy::disallowed_methods,
    reason = "the mute is a test-only process-env facade the server forwards by name"
)]
fn test_control_acks_muted() -> bool {
    std::env::var_os(EnvVars::FABRO_TEST_CONTROL_ACKS_MUTED).is_some_and(|value| value == "1")
}

/// Fabro's run tools for the run's agent sessions, when the run's settings
/// enable them and the worker token carries the scope; `None` otherwise.
/// The server issues the scope from the same setting, so the two agree
/// unless the token was issued for another run.
fn run_tool_services(worker: &PetriWorker<'_>) -> Option<FabroRunToolServices> {
    let enabled = worker.run_state.spec.settings.run.agent.fabro_tools;
    let scoped = runner::fabro_run_tools_enabled_from_worker_token(worker.worker_token);
    if !enabled || !scoped {
        info!(
            run_id = %worker.run_id,
            enabled,
            scoped,
            "Fabro's run tools are not registered on this Petri run"
        );
        return None;
    }
    let services = runner::build_fabro_run_tool_services(
        worker.worker_token,
        worker.client.clone_for_reuse(),
        worker.run_id,
    );
    if services.is_some() {
        info!(run_id = %worker.run_id, "Fabro's run tools are registered on this Petri run");
    }
    services
}

/// The runtime the worker hands Petri: no settings layer (nothing lowers
/// at execution), the model client over the worker's catalog and vault for
/// the providers whose credentials resolve, the run's mode, the Fabro
/// home the server named, and the run tools when the run has them.
async fn runtime_spec(
    vault: &Arc<AsyncRwLock<Vault>>,
    run_state: &RunProjection,
    fabro_home: Option<PathBuf>,
    run_tools: Option<FabroRunToolServices>,
) -> Result<RuntimeSpec> {
    let catalog =
        command_context::load_cli_catalog().context("failed to build worker LLM catalog")?;
    let credentials: Arc<dyn CredentialProvider> =
        Arc::new(VaultCredentialSource::new(Arc::clone(vault)));
    let ready = readiness(catalog.enabled_providers(), credentials.as_ref()).await;
    for (provider, issue) in &ready.issues {
        warn!(provider = %provider, error = %issue, "model provider credentials unusable");
    }
    let model_client = match runtime::model_client(catalog, credentials, None, &ready.ready) {
        Ok(client) => client,
        Err(err) => {
            warn!(error = %err, "Petri model client unavailable; LLM nodes run without one");
            None
        }
    };
    Ok(RuntimeSpec {
        settings_toml: None,
        mcp_catalog_toml: None,
        model_client,
        dry_run: run_state.spec.settings.run.execution.mode == RunMode::DryRun,
        fabro_home,
        run_tools,
    })
}
