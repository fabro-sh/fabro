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

mod coordinator;
mod engine;
mod model;
mod platform;
mod progress;
mod sandbox;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, TimeZone as _, Utc};
use fabro_store::StagePosition;
use fabro_store::platform_records::StoredPlatformRecord;
use fabro_types::{
    FailureReason, RunControlAction, RunDiff, RunId, RunProjection, RunStatus, StageId,
    StageProjection,
};
use petri_execution::events::{NodeRef, RunEvent, Subject};
use petri_execution::{CoordinatorEvent, CoordinatorRecord, ExecutionId};
use petri_runtime::ir::FinalizationFailure;
use petri_store::Record;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use tracing::debug;

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
    /// Stages by firing.
    #[serde(default)]
    pub stages:           BTreeMap<FiringKey, StageRef>,
    /// Labels taken, so a second firing with the same name and visit gets
    /// its own.
    #[serde(default)]
    pub labels:           BTreeSet<String>,
    #[serde(default)]
    pub invocations:      BTreeMap<u64, InvocationRef>,
    /// Which invocation each execution belongs to.
    #[serde(default)]
    pub executions:       BTreeMap<u64, u64>,
    /// Open questions by id: the firing that asked.
    #[serde(default)]
    pub questions:        BTreeMap<String, FiringKey>,
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
    /// Firings whose attempt has recorded a finish: what a position-keyed
    /// platform record may be streamed behind.
    #[serde(default)]
    pub finished_firings: BTreeSet<FiringKey>,
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
            .get(&FiringKey::new(execution.raw(), firing.raw()))?;
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

// ── Shared by the folds ─────────────────────────────────────────────────

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

/// A control the run acknowledged: the pending control is cleared when it
/// is the one that landed.
fn settle_control(projection: &mut RunProjection, action: RunControlAction) {
    if projection.pending_control == Some(action) {
        projection.pending_control = None;
    }
}

/// The key of a stage: the execution and firing of the visit it shows. The
/// same fact a positioned platform record carries as its `StagePosition`.
/// It is written `<execution>:<firing>`, which is how the stored fold
/// state keys its maps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FiringKey {
    pub execution: u64,
    pub firing:    u64,
}

impl FiringKey {
    #[must_use]
    pub fn new(execution: u64, firing: u64) -> Self {
        Self { execution, firing }
    }

    /// The firing an event belongs to: its context's execution and its
    /// subject's firing, when it has both.
    #[must_use]
    pub fn of_event(event: &RunEvent) -> Option<Self> {
        let execution = event.context.execution?;
        let firing = event.subject.as_ref()?.firing?;
        Some(Self::new(execution.raw(), firing.raw()))
    }
}

impl From<StagePosition> for FiringKey {
    fn from(position: StagePosition) -> Self {
        Self::new(position.execution, position.firing)
    }
}

impl fmt::Display for FiringKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.execution, self.firing)
    }
}

/// A firing key that is not `<execution>:<firing>`.
#[derive(Debug, thiserror::Error)]
#[error("a firing key is `<execution>:<firing>`, not {0:?}")]
pub struct ParseFiringKeyError(String);

impl FromStr for FiringKey {
    type Err = ParseFiringKeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseFiringKeyError(text.to_string());
        let (execution, firing) = text.split_once(':').ok_or_else(invalid)?;
        Ok(Self::new(
            execution.parse().map_err(|_| invalid())?,
            firing.parse().map_err(|_| invalid())?,
        ))
    }
}

impl Serialize for FiringKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FiringKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
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

fn millis(recorded_at: u64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(i64::try_from(recorded_at).unwrap_or(i64::MAX))
        .single()
        .unwrap_or_default()
}

/// The run id a Petri run key names.
#[must_use]
pub fn run_id_of(key: &str) -> Option<RunId> {
    key.parse().ok()
}

/// The required-finalization failure Fabro's hooks record when the run's
/// publication fails; its code is [`FailureReason::PublishFailed`]'s.
#[must_use]
pub fn publish_failure(message: impl Into<String>) -> FinalizationFailure {
    FinalizationFailure::new(<&'static str>::from(FailureReason::PublishFailed), message)
}

/// Whether a required-finalization failure is the run's failed publication.
#[must_use]
pub fn is_publish_failure(failure: &FinalizationFailure) -> bool {
    failure.code == <&'static str>::from(FailureReason::PublishFailed)
}

/// The committed overall status and required-finalization failure message.
/// Execution failure details remain in the invocation records and projection.
#[must_use]
pub fn finished_run_result(record: &Record) -> Option<(RunStatus, Option<String>)> {
    let record: CoordinatorRecord = serde_json::from_value(record.record.clone()).ok()?;
    match record.body {
        CoordinatorEvent::RunFinished {
            status,
            finalization_failure,
        } => Some((
            coordinator::finished_status(&status.to_string(), finalization_failure.as_ref()),
            finalization_failure.map(|failure| failure.message),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use fabro_store::platform_records::{PlatformRecord, RunCreatedRecord};
    use fabro_types::test_support as types_support;
    use petri_runtime::driver::BranchRole;
    use petri_runtime::ir::{FiringId, NodeId};

    use super::*;

    /// A stored coordinator record, as the worker's append carries one.
    fn coordinator_record(body: &serde_json::Value) -> Record {
        Record {
            seq:         3,
            recorded_at: 1_000,
            record:      serde_json::json!({
                "seq": 3,
                "origin": "external",
                "recorded_at": 1_000,
                "body": body,
            }),
        }
    }

    /// Petri's finish gives the run the status the view folds it to; any
    /// other record, or a line that is not a coordinator record, gives none.
    #[test]
    fn a_finish_record_names_the_status_the_view_ends_the_run_on() {
        let finished = |status: &str| {
            finished_run_result(&coordinator_record(&serde_json::json!({
                "event": "run.finished",
                "status": status,
            })))
            .map(|(status, _)| status)
        };
        assert_eq!(
            finished("success"),
            Some(RunStatus::Succeeded {
                reason: fabro_types::SuccessReason::Completed,
            })
        );
        assert_eq!(
            finished("cancelled"),
            Some(RunStatus::Failed {
                reason: fabro_types::FailureReason::Cancelled,
            })
        );
        assert_eq!(
            finished("failed"),
            Some(RunStatus::Failed {
                reason: fabro_types::FailureReason::WorkflowError,
            })
        );
        assert_eq!(
            finished_run_result(&coordinator_record(&serde_json::json!({
                "event": "run.paused",
            }))),
            None
        );
        assert_eq!(
            finished_run_result(&Record {
                seq:         3,
                recorded_at: 1_000,
                record:      serde_json::json!({"event": "run.finished", "status": "success"}),
            }),
            None,
            "an engine line is not a coordinator record"
        );
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

    #[test]
    fn a_firing_key_is_stored_as_execution_colon_firing() {
        let mut stages: BTreeMap<FiringKey, u32> = BTreeMap::new();
        stages.insert(FiringKey::new(3, 7), 1);
        let json = serde_json::to_string(&stages).expect("the map encodes");
        assert_eq!(json, r#"{"3:7":1}"#);
        let back: BTreeMap<FiringKey, u32> = serde_json::from_str(&json).expect("the map decodes");
        assert_eq!(back, stages);
        assert!("3-7".parse::<FiringKey>().is_err());
        assert_eq!(
            FiringKey::from(StagePosition {
                execution: 3,
                firing:    7,
            }),
            FiringKey::new(3, 7)
        );
    }
}
