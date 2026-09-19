//! The projection of a Petri run: Petri's public events and Fabro's platform
//! records folded into the view Fabro's read side serves.
//!
//! The fold is pure. [`RunView`] holds the [`RunProjection`] the API serves
//! (`GET /runs/{id}/state`, the run list through its summary) and the
//! bookkeeping the fold needs between items ([`FoldState`]): which Petri
//! firing each stage is, which invocation each execution belongs to and
//! whether it is a parallel branch, which stage asked each open question.
//! Both halves are stored by the projector and reloaded for the next pass,
//! so a pass folds only the items past the committed positions.
//!
//! The mapping follows `VIEWS.md`, row by row. The stage key is `(execution,
//! firing)`; Fabro's `StageId` (`node@visit`) is the display label the
//! `RunProjection` keys stages by, and a label two firings would share (two
//! child invocations with the same node name and visit) is made unique by
//! naming the execution. What the matrix leaves default is left default
//! here and named in the crate's README.
//!
//! Every item the fold sees carries the delivery sequence the projector
//! assigned it (`stream_seq`), which a checkpoint keeps as its `seq`. A
//! stage's `first_event_seq`, the key the stage list sorts by, is not the
//! delivery sequence: two logs' records can be committed in an order that
//! differs from their recording times by a few positions, and the view
//! built live must equal the view rebuilt from the records alone. It is the
//! milliseconds from the run's creation to the stage's `visit.started`,
//! plus one, which is the same however the records were delivered.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone as _, Utc};
use fabro_store::platform_records::{
    PlatformRecord, RunLifecycleKind, RunLifecycleRecord, StoredPlatformRecord,
};
use fabro_types::settings::run::RunEnvironmentSettings;
use fabro_types::{
    BlockedReason, CheckpointRecord as ViewCheckpoint, CodingAgentEvent, CodingEvent, Conclusion,
    FailureCategory, FailureDetail, FailureReason, InterviewOption, InterviewQuestionRecord,
    ModelRef, ModelUsage, ParallelBranchId, ParallelBranchResult, PendingInterviewRecord,
    PullRequestCreation, PullRequestCreationStatus, PullRequestLink, ReviewTarget,
    ReviewTargetKind, RunApproval, RunApprovalState, RunArtifact, RunControlAction, RunDiff,
    RunFailure, RunId, RunProjection, RunSandbox, RunSandboxFailure, RunSandboxInstance,
    RunSandboxPlan, RunSandboxRuntime, RunStatus, RunTiming, SandboxProviderKind, StageCompletion,
    StageHandler, StageId, StageInferenceProjection, StageModelUsage, StageOutcome,
    StageProjection, StageState, StageTiming, StartRecord, SuccessReason, ToolCategory, ToolSource,
    ToolSummary, first_event_seq, format_blob_ref, parse_blob_ref, timing, usage_rollup,
};
use lithos_llm::catalog::{ModelId, ProviderId};
use lithos_llm::types::Usage;
use petri_execution::events::{Derived, NodeRef, Parsed, RunEvent, Subject, ViewEvent, WaitState};
use petri_execution::{CoordinatorEvent, ExecutionId, InvocationId};
use petri_runtime::engine::{Admission, Event};
use petri_runtime::ir::{Metrics, SandboxInstance, Status, StepEvent};
use petri_runtime::steps::QuestionReference;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::interview::question_type;

/// One item the projector hands the fold, with its delivery sequence.
pub enum Item<'a> {
    Petri(&'a RunEvent),
    Platform(&'a StoredPlatformRecord),
}

/// A stage as the fold knows it: its label in the projection, and what it
/// learned about it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StageRef {
    pub stage_id:  StageId,
    /// Whether the stage is a logical one the projection shows, or a
    /// lowering node it keeps off the list.
    pub shown:     bool,
    /// The node's instance name and visit, for the collision rule.
    pub node_name: String,
    pub visit:     u32,
}

/// What the fold knows about one invocation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InvocationRef {
    /// The calling execution and firing, for a nested invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent:  Option<(u64, u64)>,
    /// The parallel group and branch index, for a branch child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch:  Option<(StageId, u32)>,
    /// The result the invocation recorded, for the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output:  Option<Value>,
}

/// Whether the run's durable record is whole, as the projector last read it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordHealth {
    pub complete:   bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<String>,
}

/// The fold's bookkeeping between items.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FoldState {
    /// Stages by `"<execution>:<firing>"`.
    #[serde(default)]
    pub stages:           BTreeMap<String, StageRef>,
    /// Labels taken, so a second firing with the same name and visit gets
    /// its own.
    #[serde(default)]
    pub labels:           BTreeSet<String>,
    #[serde(default)]
    pub invocations:      BTreeMap<u64, InvocationRef>,
    /// Which invocation each execution belongs to.
    #[serde(default)]
    pub executions:       BTreeMap<u64, u64>,
    /// Open questions by id: the stage that asked.
    #[serde(default)]
    pub questions:        BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root:             Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at:       Option<u64>,
    /// The run's recorded finish, when Petri recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished:         Option<String>,
    /// The run branch and base sha, when they arrive before `run.started`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch:       Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha:         Option<String>,
    #[serde(default)]
    pub checkpoints:      u32,
    /// The run's diff as its `run.diff` record gave it, whichever side of
    /// the run's finish it arrived on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_diff:         Option<RunDiff>,
    #[serde(default)]
    pub health:           RecordHealth,
    /// Firings (`"<execution>:<firing>"`) whose attempt has recorded a
    /// finish: what a position-keyed platform record may be streamed
    /// behind.
    #[serde(default)]
    pub finished_firings: BTreeSet<String>,
    /// Whether the run's sandbox still exists after its release
    /// (`scope.released` `retained`): kept stopped, or deleted. Absent until
    /// the root invocation's lease was released. The view carries the same
    /// fact as `RunSandboxInstance.retained`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_retained: Option<bool>,
}

impl FoldState {
    /// Whether Petri recorded the run's finish.
    #[must_use]
    pub fn finished_run(&self) -> bool {
        self.finished.is_some()
    }
}

/// The view of one run: what the API serves and what the fold keeps.
#[derive(Clone, Debug)]
pub struct RunView {
    pub projection: Option<RunProjection>,
    pub state:      FoldState,
}

impl RunView {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projection: None,
            state:      FoldState::default(),
        }
    }

    /// Fold one item at its delivery sequence.
    pub fn fold(&mut self, item: &Item<'_>, stream_seq: u64) {
        match item {
            Item::Platform(record) => self.fold_platform(record, stream_seq),
            Item::Petri(event) => self.fold_petri(event),
        }
    }

    /// The run's projection, once its `run.created` record was folded.
    #[must_use]
    pub fn projection(&self) -> Option<&RunProjection> {
        self.projection.as_ref()
    }

    // ── Platform records ────────────────────────────────────────────────

    fn fold_platform(&mut self, stored: &StoredPlatformRecord, stream_seq: u64) {
        let at = millis(stored.recorded_at);
        if let PlatformRecord::RunCreated(created) = &stored.record {
            let title = created
                .title
                .clone()
                .unwrap_or_else(|| fabro_types::infer_run_title(created.spec.graph.goal()));
            let mut projection = RunProjection::new(title, created.spec.clone(), at);
            projection.parent_id = created.parent_id;
            projection.retried_from = created.retried_from;
            projection.web_url.clone_from(&created.web_url);
            projection.sandbox = Some(RunSandbox::planned(sandbox_plan(
                &projection.spec.settings.run.environment,
            )));
            self.projection = Some(projection);
            return;
        }
        let Some(projection) = self.projection.as_mut() else {
            debug!(
                seq = stored.seq,
                kind = %stored.record.kind(),
                "platform record before run.created; not folded"
            );
            return;
        };
        touch(projection, at);
        match &stored.record {
            PlatformRecord::RunLifecycle(record) => fold_lifecycle(projection, record, at),
            PlatformRecord::RunTitle(record) => projection.title.clone_from(&record.title),
            PlatformRecord::RunParent(record) => projection.parent_id = record.parent_id,
            PlatformRecord::RunArchived => projection.archived_at = Some(at),
            PlatformRecord::RunUnarchived => projection.archived_at = None,
            PlatformRecord::RunSuperseded(record) => {
                projection.superseded_by = Some(record.new_run_id);
            }
            PlatformRecord::RunCreated(_)
            | PlatformRecord::RunNotice(_)
            | PlatformRecord::InterviewAnswered(_)
            | PlatformRecord::NotificationSent(_)
            | PlatformRecord::RunPaired(_) => {}
            PlatformRecord::RunBranch(record) => {
                self.state.run_branch.clone_from(&record.run_branch);
                self.state.base_sha.clone_from(&record.base_sha);
                if let Some(start) = projection.start.as_mut() {
                    start.run_branch.clone_from(&record.run_branch);
                    start.base_sha.clone_from(&record.base_sha);
                }
            }
            PlatformRecord::GitIdentity(record) => {
                projection.git_identity = Some(record.identity.clone());
            }
            PlatformRecord::Checkpoint(record) => {
                self.state.checkpoints = self.state.checkpoints.saturating_add(1);
                let stage = self
                    .state
                    .stages
                    .get(&stage_key(record.execution, record.firing));
                let current_node = stage.map_or_else(String::new, |stage| stage.node_name.clone());
                let stage_id = stage
                    .filter(|stage| stage.shown)
                    .map(|stage| stage.stage_id.clone());
                let checkpoint = fabro_types::Checkpoint {
                    timestamp:      at,
                    current_node:   current_node.clone(),
                    git_commit_sha: record.git_commit_sha.clone(),
                };
                // The patch stays in the blob table; the view carries its
                // reference for a reader to resolve.
                let patch = record.patch_blob.as_ref().map(format_blob_ref);
                if let Some(stage) = stage_id.and_then(|stage_id| projection.stage_mut(&stage_id)) {
                    if patch.is_some() {
                        stage.diff.clone_from(&patch);
                    }
                }
                projection.checkpoints.push(ViewCheckpoint {
                    seq: u32::try_from(stream_seq).unwrap_or(u32::MAX),
                    checkpoint,
                    diff: RunDiff {
                        patch,
                        summary: record.diff_summary,
                    },
                });
            }
            PlatformRecord::ArtifactCollected(record) => {
                let stage = self
                    .state
                    .stages
                    .get(&stage_key(record.execution, record.firing));
                let Some(stage_id) = stage.map(|stage| stage.stage_id.clone()) else {
                    debug!(
                        seq = stored.seq,
                        path = record.path,
                        "artifact record for an unknown firing; not folded"
                    );
                    return;
                };
                projection.artifacts.push(RunArtifact {
                    stage_id,
                    retry: record.attempt,
                    relative_path: record.path.clone(),
                    size: record.bytes,
                    blob: record.blob,
                });
            }
            PlatformRecord::RunDiff(record) => {
                let diff = RunDiff {
                    patch:   record.patch_blob.as_ref().map(format_blob_ref),
                    summary: record.diff_summary,
                };
                if let Some(conclusion) = projection.conclusion.as_mut() {
                    conclusion.diff = diff.clone();
                }
                self.state.run_diff = Some(diff);
            }
            PlatformRecord::PullRequestRequested(record) => {
                projection.pull_request_creation = Some(PullRequestCreation {
                    id:           record.creation_id,
                    status:       PullRequestCreationStatus::Pending,
                    model:        record.model.clone(),
                    force:        record.force,
                    requested_at: at,
                    updated_at:   at,
                    pull_request: None,
                    error:        None,
                });
            }
            PlatformRecord::PullRequestCreated(record) => {
                let link = PullRequestLink {
                    owner:  record.owner.clone(),
                    repo:   record.repo.clone(),
                    number: record.number,
                };
                projection.pull_request = Some(link.clone());
                if let Some(creation) = projection
                    .pull_request_creation
                    .as_mut()
                    .filter(|creation| creation.is_pending())
                {
                    creation.succeed(link, at);
                }
            }
            PlatformRecord::PullRequestFailed(record) => {
                if let Some(creation) =
                    projection
                        .pull_request_creation
                        .as_mut()
                        .filter(|creation| {
                            creation.is_pending()
                                && record
                                    .creation_id
                                    .is_none_or(|creation_id| creation_id == creation.id)
                        })
                {
                    creation.fail(record.error.clone(), at);
                }
            }
            PlatformRecord::PullRequestLinked(record) => {
                let link = record.link();
                projection.pull_request = Some(link.clone());
                if let Some(creation) = projection
                    .pull_request_creation
                    .as_mut()
                    .filter(|creation| creation.is_pending())
                {
                    creation.succeed(link, at);
                }
            }
            PlatformRecord::PullRequestUnlinked(_) => {
                projection.pull_request = None;
                projection.pull_request_creation = None;
            }
        }
    }

    // ── Petri events ────────────────────────────────────────────────────

    fn fold_petri(&mut self, event: &RunEvent) {
        let at = millis(event.recorded_at);
        if let Some(record) = event.coordinator() {
            self.fold_coordinator(record, event, at);
        } else if let Some(engine) = event.engine() {
            self.fold_engine(engine, event, at);
        } else if let Some(view) = event.view() {
            self.fold_view(view, event, at);
        }
        if let Some(projection) = self.projection.as_mut() {
            touch(projection, at);
        }
    }

    fn fold_coordinator(&mut self, record: &CoordinatorEvent, event: &RunEvent, at: DateTime<Utc>) {
        match record {
            CoordinatorEvent::RunStarted {
                root, forked_from, ..
            } => {
                self.state.root = Some(root.raw());
                self.state.started_at = Some(event.recorded_at);
                if let Some(projection) = self.projection.as_mut() {
                    // A fork's declaration names its source; a parse failure
                    // means the source was not a Fabro run, which the
                    // projection cannot show.
                    projection.forked_from = forked_from.as_ref().and_then(|origin| {
                        Some(fabro_types::ForkOrigin {
                            source_run_id: origin.source.as_str().parse().ok()?,
                            execution:     origin.position.execution.raw(),
                            firing:        origin.position.firing.raw(),
                            rerun_last:    origin.rerun_last,
                        })
                    });
                    apply_status(projection, RunStatus::Running, at);
                    projection.start = Some(StartRecord {
                        start_time: at,
                        run_branch: self.state.run_branch.clone(),
                        base_sha:   self.state.base_sha.clone(),
                    });
                    // The scope's sandbox is acquired next; `scope.acquired`
                    // or `scope.failed` settles it.
                    if let Some(sandbox) = projection.sandbox.take() {
                        projection.sandbox = Some(RunSandbox::initializing(sandbox.plan().clone()));
                    }
                }
            }
            CoordinatorEvent::InvocationDeclared { invocation, .. } => {
                let mut info = InvocationRef::default();
                if let Some(parent) = &event.context.parent {
                    info.parent = Some((parent.execution.raw(), parent.firing.raw()));
                    if let Some((fork_firing, index)) = branch_slot(&parent.slot) {
                        let group = self
                            .state
                            .stages
                            .get(&stage_key(parent.execution.raw(), fork_firing))
                            .map(|stage| stage.stage_id.clone());
                        if let Some(group) = group {
                            info.branch = Some((group, index));
                        }
                    }
                }
                self.state.invocations.insert(invocation.raw(), info);
            }
            CoordinatorEvent::ExecutionDeclared {
                execution,
                invocation,
                ..
            } => {
                self.state
                    .executions
                    .insert(execution.raw(), invocation.raw());
            }
            CoordinatorEvent::InvocationFinished { invocation, result } => {
                let info = self.state.invocations.entry(invocation.raw()).or_default();
                info.failure = result
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.clone());
                info.output = Some(result.output.clone());
            }
            CoordinatorEvent::RunPaused => {
                if let Some(projection) = self.projection.as_mut() {
                    let prior_block = match projection.status {
                        RunStatus::Blocked { blocked_reason } => Some(blocked_reason),
                        _ => None,
                    };
                    apply_status(projection, RunStatus::Paused { prior_block }, at);
                    if projection.pending_control == Some(RunControlAction::Pause) {
                        projection.pending_control = None;
                    }
                }
            }
            CoordinatorEvent::RunUnpaused => {
                if let Some(projection) = self.projection.as_mut() {
                    let next = match projection.status {
                        RunStatus::Paused {
                            prior_block: Some(blocked_reason),
                        } => RunStatus::Blocked { blocked_reason },
                        _ => RunStatus::Running,
                    };
                    apply_status(projection, next, at);
                    if projection.pending_control == Some(RunControlAction::Unpause) {
                        projection.pending_control = None;
                    }
                }
            }
            CoordinatorEvent::RunFinished { status } => {
                self.state.finished = Some(status.to_string());
                self.conclude(status.to_string().as_str(), at);
            }
            // ── Sandbox: the retention outcome (VIEWS.md "Sandbox") ─────────
            // The instance stays on `Run.sandbox`: it names what ran, and
            // `retained` says whether it still exists.
            CoordinatorEvent::ScopeReleased {
                invocation,
                retained,
                ..
            } => {
                if Some(invocation.raw()) == self.state.root {
                    self.state.sandbox_retained = Some(*retained);
                    if let Some(sandbox) = self
                        .projection
                        .as_mut()
                        .and_then(|projection| projection.sandbox.as_mut())
                    {
                        sandbox.set_retained(*retained);
                    }
                }
            }
            CoordinatorEvent::GraphRegistered { .. }
            | CoordinatorEvent::ExecutionFinished { .. }
            | CoordinatorEvent::InvocationCancelRequested { .. }
            | CoordinatorEvent::RunNoteRecorded { .. } => {}
        }
    }

    /// The run's conclusion, from its recorded finish and what the stages
    /// summed to.
    fn conclude(&mut self, status: &str, at: DateTime<Utc>) {
        let Some(projection) = self.projection.as_mut() else {
            return;
        };
        let root = self
            .state
            .root
            .and_then(|root| self.state.invocations.get(&root));
        let failure_message = root.and_then(|root| root.failure.clone());
        let (run_status, outcome, failure) = match status {
            "success" => (
                RunStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
                StageOutcome::Succeeded,
                None,
            ),
            "cancelled" => (
                RunStatus::Failed {
                    reason: FailureReason::Cancelled,
                },
                StageOutcome::Failed {
                    retry_requested: false,
                },
                Some(RunFailure {
                    reason: FailureReason::Cancelled,
                    detail: FailureDetail::new(
                        failure_message
                            .clone()
                            .unwrap_or_else(|| "the run was cancelled".to_string()),
                        FailureCategory::Canceled,
                    ),
                }),
            ),
            _ => (
                RunStatus::Failed {
                    reason: FailureReason::WorkflowError,
                },
                StageOutcome::Failed {
                    retry_requested: false,
                },
                Some(RunFailure {
                    reason: FailureReason::WorkflowError,
                    detail: FailureDetail::new(
                        failure_message
                            .clone()
                            .unwrap_or_else(|| "the run failed".to_string()),
                        FailureCategory::Deterministic,
                    ),
                }),
            ),
        };
        apply_status(projection, run_status, at);
        projection.pending_control = None;
        projection.pending_interviews.clear();
        let rollup = usage_rollup::usage_rollup_from_projection(projection);
        let (stages, total_retries) = rollup.conclusion_stages(projection);
        let wall_time_ms = self.state.started_at.map_or(0, |started| {
            u64::try_from(at.timestamp_millis())
                .unwrap_or(0)
                .saturating_sub(started)
        });
        let timing = RunTiming::new(
            wall_time_ms,
            rollup.timing.inference_time_ms,
            rollup.timing.tool_time_ms,
        );
        let last_checkpoint = projection.checkpoints.last();
        projection.conclusion = Some(Conclusion {
            timestamp: at,
            status: outcome,
            timing,
            failure,
            final_git_commit_sha: last_checkpoint
                .and_then(|checkpoint| checkpoint.checkpoint.git_commit_sha.clone()),
            stages,
            usage: rollup.usage_if_present(),
            total_retries,
            diff: self
                .state
                .run_diff
                .clone()
                .or_else(|| last_checkpoint.map(|checkpoint| checkpoint.diff.clone()))
                .unwrap_or_default(),
        });
    }

    fn fold_engine(&mut self, engine: &Event, event: &RunEvent, at: DateTime<Utc>) {
        let Some(execution) = event.context.execution else {
            return;
        };
        match engine {
            Event::AdmissionDecided { decision, .. } => {
                if let Admission::Skip { outcome } = decision {
                    if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                        stage.state = StageState::Skipped;
                        stage.completion = Some(StageCompletion {
                            outcome:        StageOutcome::Skipped,
                            notes:          None,
                            failure_reason: failure_message(&outcome.status),
                            timestamp:      at,
                        });
                    }
                }
            }
            Event::StepStarted { attempt, .. } => {
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    if attempt.raw() > 1 {
                        stage.clear_live_timing();
                        stage.output = None;
                        stage.output_bytes = None;
                    }
                    stage.state = StageState::Running;
                    stage.live_streaming = Some(true);
                }
            }
            Event::StepProgressRecorded { ev, .. } => {
                self.fold_progress(execution, event, ev, at);
            }
            Event::StepFinished {
                firing,
                attempt,
                outcome,
            } => {
                self.state
                    .finished_firings
                    .insert(stage_key(execution.raw(), firing.raw()));
                let is_final = matches!(
                    event.derived,
                    Some(Derived::StepFinished { is_final: true, .. })
                );
                let node_name = event
                    .subject
                    .as_ref()
                    .map(|subject| subject.node.name.to_string());
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    // The step's output: a string, or a command's `stdout`,
                    // either of which is a `blob://` reference when the
                    // step offloaded it. The reference stays as it is; the
                    // bytes it names are the live log's.
                    let output = outcome
                        .output
                        .as_str()
                        .or_else(|| outcome.output.get("stdout").and_then(Value::as_str));
                    if let Some(output) = output {
                        if parse_blob_ref(output).is_none() {
                            stage.output_bytes = Some(output.len() as u64);
                        }
                        stage.output = Some(output.to_string());
                    }
                    // A simulated step (a dry run) answers with its text.
                    let simulated = outcome
                        .output
                        .get("simulated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if simulated
                        && matches!(
                            stage.handler,
                            Some(StageHandler::Prompt | StageHandler::Agent)
                        )
                    {
                        if let Some(text) = outcome.output.get("text").and_then(Value::as_str) {
                            stage.response = Some(text.to_string());
                        }
                    }
                    // An agent's answer: the `response.<node>` the step wrote
                    // into the run context, as the prompt step writes it.
                    if stage.handler == Some(StageHandler::Agent) {
                        let response = node_name
                            .as_deref()
                            .and_then(|name| {
                                outcome
                                    .context_updates
                                    .get(format!("response.{name}").as_str())
                            })
                            .and_then(Value::as_str)
                            .or_else(|| outcome.output.as_str());
                        if let Some(response) = response {
                            stage.response = Some(response.to_string());
                        }
                    }
                    stage.live_streaming = Some(false);
                    apply_metrics(stage, &outcome.metrics);
                    if is_final {
                        stage.completion = Some(StageCompletion {
                            outcome:        stage_outcome(&outcome.status),
                            notes:          None,
                            failure_reason: failure_message(&outcome.status),
                            timestamp:      at,
                        });
                        stage.termination = Some(match outcome.status {
                            Status::TimedOut => fabro_types::CommandTermination::TimedOut,
                            Status::Cancelled => fabro_types::CommandTermination::Cancelled,
                            Status::Success
                            | Status::PartialSuccess { .. }
                            | Status::Failure(_)
                            | Status::Skipped => fabro_types::CommandTermination::Exited,
                        });
                    } else {
                        stage.state = StageState::Retrying;
                        debug!(attempt = attempt.raw(), "attempt returned; a retry follows");
                    }
                }
            }
            Event::ControlRequested { .. } => {
                if let Some(Derived::ControlRequested {
                    deliverable: true,
                    answer: Some(answer),
                }) = &event.derived
                {
                    let firing_key = event.subject.as_ref().and_then(|subject| {
                        subject
                            .firing
                            .map(|firing| stage_key(execution.raw(), firing.raw()))
                    });
                    self.close_questions(answer.question.as_deref(), firing_key.as_deref(), at);
                }
            }
            // ── Sandbox: the instance (VIEWS.md "Sandbox") ──────────────────
            // The run's sandbox is the root invocation's scope. A child
            // invocation's scope (a parallel branch) shares or owns another
            // one and is not the run's; a re-acquisition (a resume, a
            // replaced sandbox) names the current instance.
            Event::ScopeAcquired {
                sandbox,
                duration_ms,
                ..
            } => {
                if let Some(projection) = self.root_scope_projection(event) {
                    let plan = sandbox_plan_of(projection);
                    projection.sandbox = Some(RunSandbox::ready(
                        plan.clone(),
                        sandbox_instance(&plan, sandbox, *duration_ms),
                    ));
                }
            }
            Event::ScopeFailed {
                provider,
                error,
                causes,
                duration_ms,
                ..
            } => {
                if let Some(projection) = self.root_scope_projection(event) {
                    let plan = sandbox_plan_of(projection);
                    let provider = provider
                        .as_deref()
                        .and_then(provider_kind)
                        .unwrap_or_else(|| plan.provider.clone());
                    projection.sandbox = Some(RunSandbox::failed(plan, RunSandboxFailure {
                        provider:    provider.to_string(),
                        error:       error.clone(),
                        causes:      causes.clone(),
                        duration_ms: *duration_ms,
                    }));
                }
            }
            Event::ExecutionStarted { .. }
            | Event::TokenEmitted { .. }
            | Event::RoutingResolved { .. }
            | Event::RouteApplied { .. }
            | Event::RetryElapsed { .. }
            | Event::NodeExpanded { .. }
            | Event::CancelRequested { .. }
            | Event::KillRequested { .. } => {}
        }
    }

    /// The projection, when `event` is a scope record of the root
    /// invocation: the run's own sandbox, not a child invocation's.
    fn root_scope_projection(&mut self, event: &RunEvent) -> Option<&mut RunProjection> {
        let root = self.state.root?;
        if event.context.invocation.map(InvocationId::raw) != Some(root) {
            return None;
        }
        self.projection.as_mut()
    }

    fn fold_progress(
        &mut self,
        execution: ExecutionId,
        event: &RunEvent,
        ev: &StepEvent,
        at: DateTime<Utc>,
    ) {
        match ev {
            StepEvent::Log { line, .. } => {
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    let output = stage.output.get_or_insert_default();
                    output.push_str(line);
                    output.push('\n');
                    stage.output_bytes = Some(output.len() as u64);
                    stage.live_streaming = Some(true);
                }
            }
            StepEvent::Artifact { .. } => {}
            StepEvent::Custom(payload) => {
                if let Some(parsed) = event.parsed() {
                    self.fold_parsed(execution, event, parsed, at);
                    return;
                }
                let kind = payload.get("kind").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "pebble" => self.fold_pebble(execution, event, payload, at),
                    "attractor.prompt" => {
                        if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                            stage.prompt = payload
                                .get("prompt")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            let model = payload.get("model").and_then(Value::as_str);
                            if let Some(model) = model {
                                let (provider, model_id) = split_model(model);
                                stage.provider_used = Some(StageModelUsage {
                                    mode:             StageModelUsage::MODE_PROMPT.to_string(),
                                    provider:         provider.map(str::to_string),
                                    model:            Some(model_id.to_string()),
                                    reasoning_effort: None,
                                    speed:            None,
                                });
                                stage.model = model_ref(provider, model_id);
                            }
                        }
                    }
                    "attractor.prompt.completed" => {
                        if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                            stage.response = payload
                                .get("response")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            if let Some(usage) = usage_of(payload.get("usage")) {
                                stage.usage = usage;
                            }
                        }
                    }
                    "attractor.fallback.plan" => {
                        if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                            let route = payload
                                .get("routes")
                                .and_then(Value::as_array)
                                .and_then(|routes| routes.first());
                            if let Some(route) = route {
                                let provider = route.get("provider").and_then(Value::as_str);
                                let model = route.get("model").and_then(Value::as_str);
                                stage.provider_used = Some(StageModelUsage {
                                    mode:             StageModelUsage::MODE_AGENT.to_string(),
                                    provider:         provider.map(str::to_string),
                                    model:            model.map(str::to_string),
                                    reasoning_effort: None,
                                    speed:            None,
                                });
                                if let Some(model) = model {
                                    stage.model = model_ref(provider, model);
                                }
                            }
                        }
                    }
                    // The tools a native session was offered, once per
                    // session (VIEWS.md "Agent activity", tools available):
                    // the stage's list is the union over its sessions, by
                    // name, in the order the sessions listed them.
                    "attractor.tools" => {
                        if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                            let tools = payload.get("tools").and_then(Value::as_array);
                            for tool in tools.into_iter().flatten() {
                                let Some(summary) = tool_summary(tool) else {
                                    continue;
                                };
                                if !stage
                                    .agent_tools
                                    .iter()
                                    .any(|known| known.name == summary.name)
                                {
                                    stage.agent_tools.push(summary);
                                }
                            }
                        }
                    }
                    "attractor.parallel.branch.started" => {
                        let invocation = payload.get("invocation").and_then(Value::as_u64);
                        let index = payload
                            .get("index")
                            .and_then(Value::as_u64)
                            .and_then(|index| u32::try_from(index).ok());
                        let fork_firing = payload
                            .get("occurrence")
                            .and_then(|occurrence| occurrence.get("firing"))
                            .and_then(Value::as_u64);
                        if let (Some(invocation), Some(index), Some(fork_firing)) =
                            (invocation, index, fork_firing)
                        {
                            let group = self
                                .state
                                .stages
                                .get(&stage_key(execution.raw(), fork_firing))
                                .map(|stage| stage.stage_id.clone());
                            if let Some(group) = group {
                                self.state.invocations.entry(invocation).or_default().branch =
                                    Some((group, index));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn fold_parsed(
        &mut self,
        execution: ExecutionId,
        event: &RunEvent,
        parsed: &Parsed,
        at: DateTime<Utc>,
    ) {
        match parsed {
            Parsed::Question { question } => {
                let Some(subject) = event.subject.as_ref() else {
                    return;
                };
                let Some(firing) = subject.firing else {
                    return;
                };
                let key = stage_key(execution.raw(), firing.raw());
                let label = self.state.stages.get(&key).map_or_else(
                    || subject.node.name.to_string(),
                    |stage| stage.stage_id.to_string(),
                );
                self.state.questions.insert(question.id.clone(), key);
                let Some(projection) = self.projection.as_mut() else {
                    return;
                };
                projection
                    .pending_interviews
                    .insert(question.id.clone(), PendingInterviewRecord {
                        question:   InterviewQuestionRecord {
                            id:              question.id.clone(),
                            text:            question.text.clone(),
                            stage:           label,
                            question_type:   question_type(question),
                            options:         question
                                .options
                                .iter()
                                .map(|option| InterviewOption {
                                    key:         option.key.clone(),
                                    label:       option.label.clone(),
                                    description: option.description.clone(),
                                    preview:     option.preview.clone(),
                                })
                                .collect(),
                            allow_freeform:  question.freeform,
                            timeout_seconds: question
                                .timeout_ms
                                .map(|timeout| timeout as f64 / 1000.0),
                            context_display: question.context.clone(),
                            review_target:   question.reference.as_ref().and_then(review_target),
                        },
                        started_at: at,
                    });
                apply_status(
                    projection,
                    RunStatus::Blocked {
                        blocked_reason: BlockedReason::HumanInputRequired,
                    },
                    at,
                );
            }
            Parsed::QuestionExpired { expired } => {
                self.close_questions(Some(expired.question.as_str()), None, at);
            }
            Parsed::Note { .. } => {}
        }
    }

    /// Close one question by id, or every question of a firing, and unblock
    /// the run when none is left.
    fn close_questions(
        &mut self,
        question: Option<&str>,
        firing_key: Option<&str>,
        at: DateTime<Utc>,
    ) {
        let closed: Vec<String> = match (question, firing_key) {
            (Some(question), _) => vec![question.to_string()],
            (None, Some(key)) => self
                .state
                .questions
                .iter()
                .filter(|(_, asked_by)| asked_by.as_str() == key)
                .map(|(id, _)| id.clone())
                .collect(),
            (None, None) => Vec::new(),
        };
        for id in &closed {
            self.state.questions.remove(id);
        }
        let Some(projection) = self.projection.as_mut() else {
            return;
        };
        for id in &closed {
            projection.pending_interviews.remove(id);
        }
        if projection.pending_interviews.is_empty()
            && matches!(projection.status, RunStatus::Blocked { .. })
        {
            apply_status(projection, RunStatus::Running, at);
        }
    }

    fn fold_pebble(
        &mut self,
        execution: ExecutionId,
        event: &RunEvent,
        payload: &Value,
        at: DateTime<Utc>,
    ) {
        let Some(envelope) = payload.get("event") else {
            return;
        };
        let envelope: CodingAgentEvent = match serde_json::from_value(envelope.clone()) {
            Ok(envelope) => envelope,
            Err(error) => {
                debug!(error = %error, "a pebble envelope did not decode; skipped");
                return;
            }
        };
        let Some(stage) = self.stage_of(execution, event.subject.as_ref()) else {
            return;
        };
        let agent = stage.agent.get_or_insert_default();
        agent.apply(&envelope);
        if stage.completion.is_none() {
            stage.usage = agent.usage.saturating_add(agent.descendant_usage());
        }
        // A tool the stage's list names was called, by any of its sessions.
        if let CodingEvent::ToolCallStarted { tool_name, .. } = &envelope.event {
            if let Some(tool) = stage
                .agent_tools
                .iter_mut()
                .find(|tool| tool.name == *tool_name)
            {
                tool.invoked = true;
            }
        }
        let is_root = envelope.parent_session_id.is_none();
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "pebble's event vocabulary is non-exhaustive and only some events project"
        )]
        match &envelope.event {
            CodingEvent::SessionStarted {
                provider, model, ..
            } if is_root => {
                stage.provider_used = Some(StageModelUsage {
                    mode:             StageModelUsage::MODE_AGENT.to_string(),
                    provider:         provider.clone(),
                    model:            model.clone(),
                    reasoning_effort: None,
                    speed:            None,
                });
                if let Some(model) = model.as_deref() {
                    stage.model = model_ref(provider.as_deref(), model);
                }
            }
            CodingEvent::LlmRequestStarted { requested_model } if is_root => {
                stage.inference = Some(StageInferenceProjection {
                    session_id:        envelope.session_id.clone(),
                    started_at:        at,
                    requested_model:   requested_model.clone(),
                    first_output_at:   None,
                    first_output_kind: None,
                    retries:           0,
                });
            }
            CodingEvent::LlmFirstOutput { kind } => {
                if let Some(inference) = stage.inference.as_mut() {
                    if inference.session_id == envelope.session_id {
                        inference.first_output_at = Some(at);
                        inference.first_output_kind = Some(*kind);
                    }
                }
            }
            CodingEvent::LlmRetry { .. } => {
                if let Some(inference) = stage.inference.as_mut() {
                    if inference.session_id == envelope.session_id {
                        inference.retries = inference.retries.saturating_add(1);
                        inference.first_output_at = None;
                        inference.first_output_kind = None;
                    }
                }
            }
            CodingEvent::AssistantMessage { model, .. } => {
                if is_root {
                    if let Some(provider) = stage
                        .provider_used
                        .as_ref()
                        .and_then(|used| used.provider.as_deref())
                    {
                        stage.model = model_ref(Some(provider), model);
                    }
                }
                close_inference(stage, &envelope.session_id, at);
            }
            CodingEvent::Error { .. } | CodingEvent::RoundInterrupted { .. } => {
                close_inference(stage, &envelope.session_id, at);
            }
            CodingEvent::SessionEnded => {
                close_inference(stage, &envelope.session_id, at);
                stage.close_tool_batch_for_session(&envelope.session_id, at);
            }
            CodingEvent::ToolCallStarted { tool_call_id, .. } if is_root => {
                stage.open_tool_call(envelope.session_id.clone(), tool_call_id.clone(), at);
            }
            CodingEvent::ToolCallCompleted { tool_call_id, .. } if is_root => {
                stage.close_tool_call(&envelope.session_id, tool_call_id, at);
            }
            _ => {}
        }
    }

    fn fold_view(&mut self, view: &ViewEvent, event: &RunEvent, at: DateTime<Utc>) {
        let Some(execution) = event.context.execution else {
            return;
        };
        match view {
            ViewEvent::VisitStarted { .. } => {
                let Some(subject) = event.subject.as_ref() else {
                    return;
                };
                self.start_visit(execution, subject, at);
            }
            ViewEvent::WaitStateChanged { state } => {
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    match state {
                        WaitState::AwaitingAdmission => {
                            if stage.state == StageState::Running {
                                stage.state = StageState::Pending;
                            }
                        }
                        WaitState::Running | WaitState::AwaitingAnswer | WaitState::Cancelling => {
                            stage.state = StageState::Running;
                        }
                        WaitState::AwaitingRetry => stage.state = StageState::Retrying,
                    }
                }
            }
            ViewEvent::RetryScheduled { .. } => {
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    stage.state = StageState::Retrying;
                }
            }
            ViewEvent::VisitCompleted {
                outcome,
                executed,
                attempts,
            } => {
                let Some(stage) = self.stage_of(execution, event.subject.as_ref()) else {
                    return;
                };
                stage.state = match outcome.status {
                    Status::Success => StageState::Succeeded,
                    Status::PartialSuccess { .. } => StageState::PartiallySucceeded,
                    Status::Failure(_) | Status::TimedOut => StageState::Failed,
                    Status::Skipped => StageState::Skipped,
                    Status::Cancelled => StageState::Cancelled,
                };
                if stage.completion.is_none() || !*executed {
                    stage.completion = Some(StageCompletion {
                        outcome:        stage_outcome(&outcome.status),
                        notes:          None,
                        failure_reason: failure_message(&outcome.status),
                        timestamp:      at,
                    });
                }
                if stage.timing.is_none() {
                    let wall = stage
                        .started_at
                        .map_or(0, |started| timing::elapsed_ms(started, at));
                    stage.set_authoritative_timing(StageTiming::new(wall, 0, 0));
                }
                debug!(attempts, "visit completed");
            }
            ViewEvent::ForkCompleted {
                occurrence,
                results,
                ..
            } => {
                let key = stage_key(occurrence.execution.raw(), occurrence.firing.raw());
                let Some(stage_id) = self
                    .state
                    .stages
                    .get(&key)
                    .map(|stage| stage.stage_id.clone())
                else {
                    return;
                };
                let Some(projection) = self.projection.as_mut() else {
                    return;
                };
                if let Some(stage) = projection.stage_mut(&stage_id) {
                    stage.parallel_results = Some(
                        results
                            .iter()
                            .map(|result| ParallelBranchResult {
                                id:              result.node.name.to_string(),
                                index:           Some(result.branch.index as usize),
                                item_label:      None,
                                status:          stage_outcome(&result.status),
                                context_updates: BTreeMap::new(),
                            })
                            .collect(),
                    );
                }
            }
            ViewEvent::ForkStarted { .. }
            | ViewEvent::BranchCompleted { .. }
            | ViewEvent::RunStalled { .. } => {}
        }
    }

    /// A firing exists: register its stage and, when it is a logical stage,
    /// show it.
    fn start_visit(&mut self, execution: ExecutionId, subject: &Subject, at: DateTime<Utc>) {
        let Some(firing) = subject.firing else {
            return;
        };
        let key = stage_key(execution.raw(), firing.raw());
        if self.state.stages.contains_key(&key) {
            return;
        }
        let node_name = subject.node.name.to_string();
        let visit = visit_of(subject);
        let meta_kind = node_meta_kind(&subject.node);
        let shown = is_shown(&subject.node);
        // Only a shown stage takes a label: a lowering node (a branch's
        // parent-side delegate shares its target's name) never competes with
        // the stage it stands for.
        let mut stage_id = StageId::new(node_name.clone(), visit);
        if shown {
            stage_id = stage_label(&node_name, visit, execution, &self.state.labels);
            self.state.labels.insert(stage_id.to_string());
        }
        self.state.stages.insert(key, StageRef {
            stage_id: stage_id.clone(),
            shown,
            node_name,
            visit,
        });
        if !shown {
            return;
        }
        let branch = self
            .state
            .executions
            .get(&execution.raw())
            .and_then(|invocation| self.state.invocations.get(invocation))
            .and_then(|invocation| invocation.branch.clone());
        let Some(projection) = self.projection.as_mut() else {
            return;
        };
        let since_created = at
            .signed_duration_since(projection.spec.run_id.created_at())
            .num_milliseconds()
            .max(0);
        let ordinal = u32::try_from(since_created)
            .unwrap_or(u32::MAX - 1)
            .saturating_add(1);
        let stage = projection.stage_entry(stage_id.node_id(), visit, first_event_seq(ordinal));
        stage.handler = Some(StageHandler::from_handler_type(Some(meta_kind)));
        stage.started_at = Some(at);
        stage.graph_visit = Some(visit);
        stage.state = StageState::Pending;
        stage.parallel_branch_id = branch.map(|(group, index)| ParallelBranchId::new(group, index));
    }

    /// The shown stage an event's subject firing belongs to.
    fn stage_of(
        &mut self,
        execution: ExecutionId,
        subject: Option<&Subject>,
    ) -> Option<&mut StageProjection> {
        let firing = subject?.firing?;
        let stage = self
            .state
            .stages
            .get(&stage_key(execution.raw(), firing.raw()))?;
        if !stage.shown {
            return None;
        }
        let stage_id = stage.stage_id.clone();
        self.projection.as_mut()?.stage_mut(&stage_id)
    }
}

impl Default for RunView {
    fn default() -> Self {
        Self::new()
    }
}

// ── Lifecycle ───────────────────────────────────────────────────────────

fn fold_lifecycle(projection: &mut RunProjection, record: &RunLifecycleRecord, at: DateTime<Utc>) {
    use RunLifecycleKind as Kind;
    match record.transition {
        Kind::Submitted => apply_status(projection, RunStatus::Submitted, at),
        Kind::StartRequested => {}
        Kind::Unpaused => {
            let status = match projection.status {
                RunStatus::Paused {
                    prior_block: Some(blocked_reason),
                } => RunStatus::Blocked { blocked_reason },
                _ => RunStatus::Running,
            };
            apply_status(projection, status, at);
            if projection.pending_control == Some(RunControlAction::Unpause) {
                projection.pending_control = None;
            }
        }
        Kind::Pending => {
            if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
            projection.approval = Some(RunApproval {
                state:         RunApprovalState::Pending,
                requested_at:  at,
                decided_at:    None,
                denial_reason: None,
            });
        }
        Kind::Approved => {
            if let Some(approval) = projection.approval.as_mut() {
                approval.state = RunApprovalState::Approved;
                approval.decided_at = Some(at);
            }
        }
        Kind::Denied => {
            if let Some(approval) = projection.approval.as_mut() {
                approval.state = RunApprovalState::Denied;
                approval.decided_at = Some(at);
                approval.denial_reason.clone_from(&record.reason);
            }
            apply_status(
                projection,
                RunStatus::Failed {
                    reason: FailureReason::ApprovalDenied,
                },
                at,
            );
        }
        Kind::Runnable => {
            // A run left in flight by a restart goes back to the queue: the
            // resume's `runnable` steps back from wherever the run stood.
            let in_flight = matches!(
                projection.status,
                RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. }
            );
            if in_flight && record.status == Some(RunStatus::Runnable) {
                projection.status = RunStatus::Runnable;
                projection.status_updated_at = at;
            } else if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
        }
        Kind::Blocked => {
            // A block that lands while the run is paused waits behind the
            // pause: the unpause restores it.
            match (projection.status, record.status) {
                (RunStatus::Paused { .. }, Some(RunStatus::Blocked { blocked_reason })) => {
                    apply_status(
                        projection,
                        RunStatus::Paused {
                            prior_block: Some(blocked_reason),
                        },
                        at,
                    );
                }
                (_, Some(status)) => apply_status(projection, status, at),
                (_, None) => {}
            }
        }
        Kind::Unblocked => {
            let status = match projection.status {
                RunStatus::Paused { .. } => RunStatus::Paused { prior_block: None },
                _ => record.status.unwrap_or(RunStatus::Running),
            };
            apply_status(projection, status, at);
        }
        Kind::Starting | Kind::Running | Kind::Removing | Kind::Dead => {
            if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
        }
        Kind::Paused => {
            let prior_block = match projection.status {
                RunStatus::Blocked { blocked_reason } => Some(blocked_reason),
                RunStatus::Paused { prior_block } => prior_block,
                _ => None,
            };
            apply_status(projection, RunStatus::Paused { prior_block }, at);
            if projection.pending_control == Some(RunControlAction::Pause) {
                projection.pending_control = None;
            }
        }
        Kind::Succeeded | Kind::Failed => {
            if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
            projection.pending_control = None;
            if projection.conclusion.is_none() {
                let (outcome, failure) = match record.status {
                    Some(RunStatus::Failed { reason }) => (
                        StageOutcome::Failed {
                            retry_requested: false,
                        },
                        Some(RunFailure {
                            reason,
                            detail: FailureDetail::new(
                                record
                                    .reason
                                    .clone()
                                    .unwrap_or_else(|| "the run failed".to_string()),
                                FailureCategory::Deterministic,
                            ),
                        }),
                    ),
                    _ => (StageOutcome::Succeeded, None),
                };
                projection.conclusion = Some(Conclusion {
                    timestamp: at,
                    status: outcome,
                    timing: RunTiming::default(),
                    failure,
                    final_git_commit_sha: None,
                    stages: Vec::new(),
                    usage: None,
                    total_retries: 0,
                    diff: RunDiff::default(),
                });
            }
        }
        Kind::CancelRequested => projection.pending_control = Some(RunControlAction::Cancel),
        Kind::PauseRequested => projection.pending_control = Some(RunControlAction::Pause),
        Kind::UnpauseRequested => projection.pending_control = Some(RunControlAction::Unpause),
    }
}

/// Apply a status transition; one the lifecycle refuses is logged and
/// skipped, since the view never fails the run.
fn apply_status(projection: &mut RunProjection, status: RunStatus, at: DateTime<Utc>) {
    if let Err(error) = projection.try_apply_status(status, at) {
        debug!(error = %error, "status transition not applied to the Petri projection");
    }
}

fn touch(projection: &mut RunProjection, at: DateTime<Utc>) {
    if at > projection.last_event_at {
        projection.last_event_at = at;
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// The key of a stage: its execution and firing.
#[must_use]
pub fn stage_key(execution: u64, firing: u64) -> String {
    format!("{execution}:{firing}")
}

/// Which firing of its node a subject is, 1-based.
#[must_use]
pub fn visit_of(subject: &Subject) -> u32 {
    subject.visit.unwrap_or(1).max(1)
}

/// The role a frontend gave a node under `meta.kind`, or the empty string.
fn node_meta_kind(node: &NodeRef) -> &str {
    node.meta.get("kind").and_then(Value::as_str).unwrap_or("")
}

/// Whether a node is a logical stage the projection shows, or a lowering
/// node it keeps off the list: one a frontend marked synthetic, or a
/// parallel branch's delegate.
#[must_use]
pub fn is_shown(node: &NodeRef) -> bool {
    let synthetic = node
        .meta
        .get("synthetic")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    !synthetic && node_meta_kind(node) != "parallel.branch"
}

/// The label a shown firing takes, which is the stage id the projection
/// keys it by: `node@visit`, or `node/e<execution>@visit` when another
/// execution's firing already took that label. `taken` is every label given
/// so far; the caller adds the one returned. The interview adapter labels a
/// question's stage through this same rule, so the stage a question names
/// is the stage the projection shows.
#[must_use]
pub fn stage_label(
    node_name: &str,
    visit: u32,
    execution: ExecutionId,
    taken: &BTreeSet<String>,
) -> StageId {
    let stage_id = StageId::new(node_name.to_string(), visit);
    if taken.contains(&stage_id.to_string()) {
        return StageId::new(format!("{node_name}/e{}", execution.raw()), visit);
    }
    stage_id
}

/// The fork firing and branch index a branch child's call slot names:
/// `branch:<fork>@<firing>:<index>:<target>`.
fn branch_slot(slot: &str) -> Option<(u64, u32)> {
    let rest = slot.strip_prefix("branch:")?;
    let mut parts = rest.splitn(3, ':');
    let fork = parts.next()?;
    let index = parts.next()?.parse::<u32>().ok()?;
    let firing = fork.rsplit_once('@')?.1.parse::<u64>().ok()?;
    Some((firing, index))
}

fn millis(recorded_at: u64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(i64::try_from(recorded_at).unwrap_or(i64::MAX))
        .single()
        .unwrap_or_default()
}

fn sandbox_plan(settings: &RunEnvironmentSettings) -> RunSandboxPlan {
    RunSandboxPlan {
        provider: settings.provider.clone(),
        image:    (settings.provider == SandboxProviderKind::DOCKER)
            .then(|| settings.image.docker.clone())
            .flatten()
            .filter(|image| !image.is_empty()),
        snapshot: None,
    }
}

/// The plan the projection's sandbox carries, or the one its environment
/// settings give when no sandbox was projected yet.
fn sandbox_plan_of(projection: &RunProjection) -> RunSandboxPlan {
    projection.sandbox.as_ref().map_or_else(
        || sandbox_plan(&projection.spec.settings.run.environment),
        |sandbox| sandbox.plan().clone(),
    )
}

/// Fabro's name for the provider Petri's `scope.acquired` names: Petri's
/// `host` is Fabro's `local`; every other kind is spelled the same. `None`
/// for a name that is no provider kind.
fn provider_kind(provider: &str) -> Option<SandboxProviderKind> {
    if provider == "host" {
        return Some(SandboxProviderKind::LOCAL);
    }
    SandboxProviderKind::try_new(provider).ok()
}

/// The run's sandbox instance from Petri's record of the scope's
/// acquisition: the provider, the provider's id for the sandbox (what a
/// reconnect attaches by), its image and snapshot when the provider knows
/// them, the working directory, and how long the acquisition took. The
/// clone fields stay unset: Petri's checkout copies the bound repository
/// into the workspace and is not a clone Fabro made, and the workspace
/// roots are the provider's own layout, read live. `retained` waits for
/// the scope's release.
fn sandbox_instance(
    plan: &RunSandboxPlan,
    sandbox: &SandboxInstance,
    ready_duration_ms: u64,
) -> RunSandboxInstance {
    RunSandboxInstance {
        provider:          provider_kind(&sandbox.provider)
            .unwrap_or_else(|| plan.provider.clone()),
        image:             sandbox
            .image
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| plan.image.clone()),
        snapshot:          sandbox.snapshot.as_ref().map(ToString::to_string),
        runtime:           RunSandboxRuntime {
            id:                sandbox.instance.to_string(),
            working_directory: sandbox.working_directory.to_string(),
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
            workspace_root:    None,
            repos_root:        None,
            primary_repo_path: None,
            primary_repo_link: None,
        },
        ready_duration_ms: Some(ready_duration_ms),
        retained:          None,
    }
}

fn stage_outcome(status: &Status) -> StageOutcome {
    match status {
        Status::Success => StageOutcome::Succeeded,
        Status::PartialSuccess { .. } => StageOutcome::PartiallySucceeded,
        Status::Failure(info) => StageOutcome::Failed {
            retry_requested: info.class.as_str() == "retry_requested",
        },
        Status::Skipped => StageOutcome::Skipped,
        Status::Cancelled | Status::TimedOut => StageOutcome::Failed {
            retry_requested: false,
        },
    }
}

fn failure_message(status: &Status) -> Option<String> {
    match status {
        Status::Failure(info)
        | Status::PartialSuccess {
            underlying: Some(info),
        } => Some(info.message.clone()),
        Status::TimedOut => Some("the step timed out".to_string()),
        Status::Cancelled => Some("the step was cancelled".to_string()),
        Status::Success | Status::PartialSuccess { underlying: None } | Status::Skipped => None,
    }
}

/// The finished attempt's metrics onto its stage: the timing and the usage
/// the backend reported.
/// One tool of an `attractor.tools` payload as the stage's list carries
/// it: the name and description as recorded, Pebble's `source` as it is,
/// and Pebble's behavioural category where Petri's says which (a
/// sub-agent tool); every other tool is `other`, because the payload
/// carries Petri's origin category (`builtin`, `mcp`, `host`, `question`),
/// not Pebble's permission class. `invoked` starts false and flips on the
/// session's `ToolCallStarted`.
fn tool_summary(tool: &Value) -> Option<ToolSummary> {
    let name = tool.get("name").and_then(Value::as_str)?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = tool
        .get("source")
        .cloned()
        .and_then(|source| serde_json::from_value::<ToolSource>(source).ok())
        .unwrap_or_default();
    let category = match tool.get("category").and_then(Value::as_str) {
        Some("subagent") => ToolCategory::Subagent,
        _ => ToolCategory::Other,
    };
    Some(ToolSummary {
        name: name.to_string(),
        description: description.to_string(),
        source,
        category,
        invoked: false,
    })
}

/// The question's `reference` as Fabro's review target, when it is one
/// Fabro's validation admits (a `document`, or a reference without a kind,
/// with a label and an absolute HTTP URL within Fabro's limits).
fn review_target(reference: &QuestionReference) -> Option<ReviewTarget> {
    let kind = match reference.kind.as_deref() {
        Some("document") | None => ReviewTargetKind::Document,
        Some(_) => return None,
    };
    ReviewTarget::new(reference.label.clone(), reference.url.clone(), kind).ok()
}

fn apply_metrics(stage: &mut StageProjection, metrics: &Metrics) {
    let custom = &metrics.custom;
    let inference = custom
        .get("pebble.inference_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let tool = custom
        .get("pebble.tool_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let wall = metrics.duration_ms.unwrap_or(0);
    let (inference, tool) = match stage.handler {
        Some(StageHandler::Prompt) => (wall, 0),
        Some(StageHandler::Command) => (0, wall),
        _ => (inference, tool),
    };
    stage.set_authoritative_timing(StageTiming::new(wall, inference, tool).clamped_to_wall());
    if let Some(usage) =
        usage_of(custom.get("pebble.usage")).or_else(|| usage_of(custom.get("prompt.usage")))
    {
        stage.usage = usage;
    }
    if let Some(sessions) = custom
        .get("pebble.subagents")
        .and_then(|subagents| subagents.get("sessions"))
        .and_then(Value::as_array)
    {
        let mut by_model: Vec<ModelUsage> = Vec::new();
        for session in sessions {
            let provider = session.get("provider").and_then(Value::as_str);
            let model = session.get("model").and_then(Value::as_str);
            let Some(usage) = usage_of(session.get("usage")) else {
                continue;
            };
            let Some(model) = model.and_then(|model| model_ref(provider, model)) else {
                continue;
            };
            if let Some(entry) = by_model.iter_mut().find(|entry| entry.model == model) {
                entry.usage = entry.usage.saturating_add(usage);
            } else {
                by_model.push(ModelUsage::new(model, usage));
            }
        }
        if !by_model.is_empty() {
            stage.usage_by_model = by_model;
        }
    }
}

fn usage_of(value: Option<&Value>) -> Option<Usage> {
    serde_json::from_value(value?.clone()).ok()
}

/// `provider/model` into its parts, or the model alone.
fn split_model(model: &str) -> (Option<&str>, &str) {
    match model.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
            (Some(provider), model)
        }
        _ => (None, model),
    }
}

fn model_ref(provider: Option<&str>, model: &str) -> Option<ModelRef> {
    let provider = provider.filter(|provider| !provider.is_empty())?;
    Some(ModelRef::new(
        ProviderId::new(provider),
        ModelId::new(model),
    ))
}

fn close_inference(stage: &mut StageProjection, session_id: &str, at: DateTime<Utc>) {
    let open = stage
        .inference
        .as_ref()
        .is_some_and(|inference| inference.session_id == session_id);
    if !open {
        return;
    }
    if let Some(inference) = stage.inference.take() {
        stage.accumulate_inference_ms(timing::elapsed_ms(inference.started_at, at));
    }
}

/// The run id a Petri run key names.
#[must_use]
pub fn run_id_of(key: &str) -> Option<RunId> {
    key.parse().ok()
}

#[cfg(test)]
mod tests {
    use fabro_store::platform_records::RunCreatedRecord;
    use fabro_types::test_support as types_support;
    use petri_runtime::driver::BranchRole;
    use petri_runtime::ir::{FiringId, NodeId};

    use super::*;

    #[test]
    fn a_branch_slot_names_the_fork_firing_and_the_index() {
        assert_eq!(branch_slot("branch:fan@7:2:review"), Some((7, 2)));
        assert_eq!(branch_slot("branch:fan@7:x:review"), None);
        assert_eq!(branch_slot("child:0"), None);
    }

    #[test]
    fn a_model_selector_splits_into_provider_and_model() {
        assert_eq!(split_model("openai/gpt-5.4"), (Some("openai"), "gpt-5.4"));
        assert_eq!(split_model("gpt-5.4"), (None, "gpt-5.4"));
        assert!(model_ref(None, "gpt-5.4").is_none());
        assert!(model_ref(Some("openai"), "gpt-5.4").is_some());
    }

    #[test]
    fn a_taken_label_is_made_unique_by_the_execution() {
        let mut view = RunView::new();
        let created = StoredPlatformRecord {
            seq:         1,
            recorded_at: 1_000,
            record:      PlatformRecord::RunCreated(RunCreatedRecord {
                spec:         types_support::test_run_spec(),
                title:        Some("A run".to_string()),
                parent_id:    None,
                retried_from: None,
                web_url:      None,
            }),
            position:    None,
        };
        view.fold(&Item::Platform(&created), 1);
        let subject = |name: &str| Subject {
            node:       NodeRef {
                id:   NodeId::new(1),
                name: name.into(),
                kind: "attractor/command".into(),
                meta: serde_json::json!({ "kind": "command" }),
            },
            firing:     Some(FiringId::new(4)),
            visit:      Some(1),
            attempt:    None,
            generation: None,
            branch:     BranchRole::None,
        };
        view.start_visit(ExecutionId::new(1), &subject("build"), millis(2_000));
        view.start_visit(ExecutionId::new(2), &subject("build"), millis(3_000));
        let labels: Vec<String> = view
            .projection()
            .expect("the run was created")
            .iter_stages()
            .map(|(id, _)| id.to_string())
            .collect();
        assert_eq!(labels, vec!["build@1", "build/e2@1"]);
        assert_eq!(view.state.stages.len(), 2);
    }
}
