//! Fabro's own facts about a Petri run: the platform records.
//!
//! Petri's records are a Petri run's source of truth for everything the
//! engine did. What Fabro itself does for a run (its lifecycle before and
//! after the engine, a checkpoint commit, a pull request, a notification, a
//! pairing) is not a Petri record. Those facts live here, in the
//! `platform_records` table, one row per fact, keyed by `(run_id, seq)`
//! with `seq` per run assigned by the store, and tied to a Petri stage
//! through `(execution, firing)` when they belong to one.
//!
//! [`PlatformRecord`] is the one enum of record kinds, each with its typed
//! payload, tagged by `kind` on the wire; [`PlatformRecordKind`] names the
//! kinds. The writer of a record is whoever performs the effect: the
//! `run.created` and `run.lifecycle` kinds by the run's create and lifecycle
//! paths (the server at create, the worker around the engine), through
//! [`PlatformRecordStore::append`] or the worker's client. The
//! `run.branch`, `git.identity`, `checkpoint`, `artifact.collected`,
//! `run.diff`, `pull_request.created`, `notification.sent` and `run.paired`
//! kinds are defined here and written by the adapters that perform those
//! effects.
//!
//! Every record may carry an [`OperationKey`]: the identity of the external
//! effect it records (the execution, the Petri decision and the effect
//! kind), so the record and the effect share one identity and a retry after
//! a crash finds the effect already done.

use std::sync::Arc;

use fabro_types::{
    BlobHash, DiffSummary, GitIdentity, PairId, PairTarget, Principal, PullRequestCreationId,
    PullRequestLink, RunControlAction, RunId, RunNoticeLevel, RunSpec, RunStatus,
};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnection, SqliteRow};
use sqlx::{Row as _, SqlitePool};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::{Error, Result};

/// What the run summary store calls after it commits a platform record for
/// a run: the server's wake-up for the run's projector.
pub type PlatformRecordHook = Arc<dyn Fn(RunId) + Send + Sync>;

/// The Petri stage a platform record belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagePosition {
    pub execution: u64,
    pub firing:    u64,
}

/// A Petri decision, as the engine's `DecisionId` names it on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRef {
    ExecutionStart,
    AttemptStart { firing: u64, attempt: u32 },
    Route { firing: u64, attempt: u32 },
}

/// The identity of one external effect Fabro performed for a run: the
/// execution, the Petri decision it was performed under, and the effect
/// kind (`commit`, `push`, `pull_request`, `child_run`, ...). The run key is
/// the record's run. An effect is performed at most once per key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationKey {
    pub execution: u64,
    pub decision:  DecisionRef,
    pub effect:    String,
}

/// The kinds of platform record, as their `kind` tags spell them.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
pub enum PlatformRecordKind {
    #[serde(rename = "run.created")]
    #[strum(serialize = "run.created")]
    RunCreated,
    #[serde(rename = "run.lifecycle")]
    #[strum(serialize = "run.lifecycle")]
    RunLifecycle,
    #[serde(rename = "run.title")]
    #[strum(serialize = "run.title")]
    RunTitle,
    #[serde(rename = "run.parent")]
    #[strum(serialize = "run.parent")]
    RunParent,
    #[serde(rename = "run.archived")]
    #[strum(serialize = "run.archived")]
    RunArchived,
    #[serde(rename = "run.unarchived")]
    #[strum(serialize = "run.unarchived")]
    RunUnarchived,
    #[serde(rename = "run.superseded")]
    #[strum(serialize = "run.superseded")]
    RunSuperseded,
    #[serde(rename = "run.notice")]
    #[strum(serialize = "run.notice")]
    RunNotice,
    #[serde(rename = "interview.answered")]
    #[strum(serialize = "interview.answered")]
    InterviewAnswered,
    #[serde(rename = "run.branch")]
    #[strum(serialize = "run.branch")]
    RunBranch,
    #[serde(rename = "git.identity")]
    #[strum(serialize = "git.identity")]
    GitIdentity,
    #[serde(rename = "checkpoint")]
    #[strum(serialize = "checkpoint")]
    Checkpoint,
    #[serde(rename = "artifact.collected")]
    #[strum(serialize = "artifact.collected")]
    ArtifactCollected,
    #[serde(rename = "run.diff")]
    #[strum(serialize = "run.diff")]
    RunDiff,
    #[serde(rename = "pull_request.requested")]
    #[strum(serialize = "pull_request.requested")]
    PullRequestRequested,
    #[serde(rename = "pull_request.created")]
    #[strum(serialize = "pull_request.created")]
    PullRequestCreated,
    #[serde(rename = "pull_request.failed")]
    #[strum(serialize = "pull_request.failed")]
    PullRequestFailed,
    #[serde(rename = "pull_request.linked")]
    #[strum(serialize = "pull_request.linked")]
    PullRequestLinked,
    #[serde(rename = "pull_request.unlinked")]
    #[strum(serialize = "pull_request.unlinked")]
    PullRequestUnlinked,
    #[serde(rename = "notification.sent")]
    #[strum(serialize = "notification.sent")]
    NotificationSent,
    #[serde(rename = "run.paired")]
    #[strum(serialize = "run.paired")]
    RunPaired,
}

/// One platform record, tagged by `kind` on the wire.
#[allow(
    clippy::large_enum_variant,
    reason = "the created record carries the run spec, as the run's first event does"
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PlatformRecord {
    /// The run exists: the spec Fabro built for it.
    #[serde(rename = "run.created")]
    RunCreated(RunCreatedRecord),
    /// A lifecycle transition Fabro decided before, beside or after the
    /// engine: the queue, approval, a control request, the terminal status
    /// Fabro reports.
    #[serde(rename = "run.lifecycle")]
    RunLifecycle(RunLifecycleRecord),
    #[serde(rename = "run.title")]
    RunTitle(RunTitleRecord),
    #[serde(rename = "run.parent")]
    RunParent(RunParentRecord),
    #[serde(rename = "run.archived")]
    RunArchived,
    #[serde(rename = "run.unarchived")]
    RunUnarchived,
    #[serde(rename = "run.superseded")]
    RunSuperseded(RunSupersededRecord),
    #[serde(rename = "run.notice")]
    RunNotice(RunNoticeRecord),
    /// Who answered a question, beside the answer Petri recorded.
    #[serde(rename = "interview.answered")]
    InterviewAnswered(InterviewAnsweredRecord),
    /// The run branch and base commit Fabro created for the run.
    #[serde(rename = "run.branch")]
    RunBranch(RunBranchRecord),
    #[serde(rename = "git.identity")]
    GitIdentity(GitIdentityRecord),
    /// A stage's files committed on the run branch: the position-to-snapshot
    /// record, written after the commit succeeds.
    #[serde(rename = "checkpoint")]
    Checkpoint(CheckpointRecord),
    /// A file a stage's attempt left in its workspace, collected under
    /// `[run.artifacts] include` into the blob table.
    #[serde(rename = "artifact.collected")]
    ArtifactCollected(ArtifactCollectedRecord),
    /// The run's whole diff, its run branch against its base commit, written
    /// when the run finishes.
    #[serde(rename = "run.diff")]
    RunDiff(RunDiffRecord),
    /// A pull request was asked for: the supervisor creates it.
    #[serde(rename = "pull_request.requested")]
    PullRequestRequested(PullRequestRequestedRecord),
    #[serde(rename = "pull_request.created")]
    PullRequestCreated(PullRequestCreatedRecord),
    /// The requested pull request could not be created.
    #[serde(rename = "pull_request.failed")]
    PullRequestFailed(PullRequestFailedRecord),
    /// An existing pull request was linked to the run by hand.
    #[serde(rename = "pull_request.linked")]
    PullRequestLinked(PullRequestLinkedRecord),
    #[serde(rename = "pull_request.unlinked")]
    PullRequestUnlinked(PullRequestLinkedRecord),
    #[serde(rename = "notification.sent")]
    NotificationSent(NotificationSentRecord),
    #[serde(rename = "run.paired")]
    RunPaired(RunPairedRecord),
}

impl PlatformRecord {
    #[must_use]
    pub fn kind(&self) -> PlatformRecordKind {
        match self {
            Self::RunCreated(_) => PlatformRecordKind::RunCreated,
            Self::RunLifecycle(_) => PlatformRecordKind::RunLifecycle,
            Self::RunTitle(_) => PlatformRecordKind::RunTitle,
            Self::RunParent(_) => PlatformRecordKind::RunParent,
            Self::RunArchived => PlatformRecordKind::RunArchived,
            Self::RunUnarchived => PlatformRecordKind::RunUnarchived,
            Self::RunSuperseded(_) => PlatformRecordKind::RunSuperseded,
            Self::RunNotice(_) => PlatformRecordKind::RunNotice,
            Self::InterviewAnswered(_) => PlatformRecordKind::InterviewAnswered,
            Self::RunBranch(_) => PlatformRecordKind::RunBranch,
            Self::GitIdentity(_) => PlatformRecordKind::GitIdentity,
            Self::Checkpoint(_) => PlatformRecordKind::Checkpoint,
            Self::ArtifactCollected(_) => PlatformRecordKind::ArtifactCollected,
            Self::RunDiff(_) => PlatformRecordKind::RunDiff,
            Self::PullRequestRequested(_) => PlatformRecordKind::PullRequestRequested,
            Self::PullRequestCreated(_) => PlatformRecordKind::PullRequestCreated,
            Self::PullRequestFailed(_) => PlatformRecordKind::PullRequestFailed,
            Self::PullRequestLinked(_) => PlatformRecordKind::PullRequestLinked,
            Self::PullRequestUnlinked(_) => PlatformRecordKind::PullRequestUnlinked,
            Self::NotificationSent(_) => PlatformRecordKind::NotificationSent,
            Self::RunPaired(_) => PlatformRecordKind::RunPaired,
        }
    }

    /// The operation identity the record carries, when it records an
    /// external effect.
    #[must_use]
    pub fn operation(&self) -> Option<&OperationKey> {
        match self {
            Self::Checkpoint(record) => record.operation.as_ref(),
            Self::ArtifactCollected(record) => record.operation.as_ref(),
            Self::PullRequestCreated(record) => record.operation.as_ref(),
            Self::NotificationSent(record) => record.operation.as_ref(),
            Self::RunCreated(_)
            | Self::RunLifecycle(_)
            | Self::RunTitle(_)
            | Self::RunParent(_)
            | Self::RunArchived
            | Self::RunUnarchived
            | Self::RunSuperseded(_)
            | Self::RunNotice(_)
            | Self::InterviewAnswered(_)
            | Self::RunBranch(_)
            | Self::GitIdentity(_)
            | Self::RunDiff(_)
            | Self::PullRequestRequested(_)
            | Self::PullRequestFailed(_)
            | Self::PullRequestLinked(_)
            | Self::PullRequestUnlinked(_)
            | Self::RunPaired(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCreatedRecord {
    pub spec:         RunSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id:    Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_from: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url:      Option<String>,
}

/// Which lifecycle transition a `run.lifecycle` record is.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RunLifecycleKind {
    Submitted,
    StartRequested,
    Pending,
    Approved,
    Denied,
    Runnable,
    Starting,
    Running,
    Blocked,
    Unblocked,
    Paused,
    Unpaused,
    Removing,
    Succeeded,
    Failed,
    Dead,
    CancelRequested,
    PauseRequested,
    UnpauseRequested,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLifecycleRecord {
    /// Which transition this is. Named apart from the record's `kind` tag.
    pub transition: RunLifecycleKind,
    /// The status the transition leads to, for a transition that is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status:     Option<RunStatus>,
    /// Why: a denial's reason, a failure's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason:     Option<String>,
    /// What made the run runnable, or whether a start request is a resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source:     Option<String>,
    /// The control a `*_requested` transition asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action:     Option<RunControlAction>,
}

impl RunLifecycleRecord {
    #[must_use]
    pub fn new(transition: RunLifecycleKind) -> Self {
        Self {
            transition,
            status: None,
            reason: None,
            source: None,
            action: None,
        }
    }

    #[must_use]
    pub fn with_status(mut self, status: RunStatus) -> Self {
        self.status = Some(status);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTitleRecord {
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunParentRecord {
    /// The parent after the change; absent when the link was removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id:          Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_parent_id: Option<RunId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSupersededRecord {
    pub new_run_id:                RunId,
    pub target_checkpoint_ordinal: usize,
    pub target_node_id:            String,
    pub target_visit:              usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunNoticeRecord {
    pub level:   RunNoticeLevel,
    pub code:    String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterviewAnsweredRecord {
    /// The question's id, as Petri's `parsed.question` names it.
    pub question:  String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<Principal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel:   Option<String>,
    /// The question's text, for a reader that shows the answer beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text:      Option<String>,
    /// The answer as the person gave it, rendered as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBranchRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha:   Option<String>,
    /// The Petri workspace the branch was created in: where the run's diff
    /// is measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace:  Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIdentityRecord {
    #[serde(flatten)]
    pub identity: GitIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub execution:      u64,
    pub firing:         u64,
    /// The attempt whose files the commit holds; absent on a record written
    /// before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt:        Option<u32>,
    /// The Petri workspace id the commit was made in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary:   Option<DiffSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_blob:     Option<BlobHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation:      Option<OperationKey>,
}

/// One file collected from a stage's workspace after its attempt finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCollectedRecord {
    pub execution: u64,
    pub firing:    u64,
    /// The attempt whose workspace the file was read from, 1-based.
    pub attempt:   u32,
    /// The file's path relative to the workspace root.
    pub path:      String,
    /// The blob that holds the file's bytes.
    pub blob:      BlobHash,
    pub bytes:     u64,
    /// The SHA-256 of the bytes as lowercase hex: with `path`, the identity
    /// a later capture of the same unchanged file is matched by.
    pub digest:    String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationKey>,
}

/// The run's diff: its run branch's head against its base commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDiffRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<DiffSummary>,
    /// The patch as a text blob; absent when the diff is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_blob:   Option<BlobHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestCreatedRecord {
    pub number:    u64,
    pub owner:     String,
    pub repo:      String,
    pub html_url:  String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha:  Option<String>,
    #[serde(default)]
    pub draft:     bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRequestedRecord {
    pub creation_id: PullRequestCreationId,
    pub model:       String,
    #[serde(default)]
    pub force:       bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestFailedRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_id: Option<PullRequestCreationId>,
    pub error:       String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestLinkedRecord {
    pub owner:  String,
    pub repo:   String,
    pub number: u64,
}

impl PullRequestLinkedRecord {
    #[must_use]
    pub fn link(&self) -> PullRequestLink {
        PullRequestLink {
            owner:  self.owner.clone(),
            repo:   self.repo.clone(),
            number: self.number,
        }
    }

    #[must_use]
    pub fn from_link(link: &PullRequestLink) -> Self {
        Self {
            owner:  link.owner.clone(),
            repo:   link.repo.clone(),
            number: link.number,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSentRecord {
    pub route:      String,
    pub event:      String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation:  Option<OperationKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunPairedRecord {
    pub pair_id: PairId,
    pub target:  PairTarget,
}

/// A platform record as the store holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPlatformRecord {
    pub seq:         u64,
    /// Milliseconds since the Unix epoch when the record was stored.
    pub recorded_at: u64,
    pub record:      PlatformRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position:    Option<StagePosition>,
}

/// The `platform_records` table.
#[derive(Clone)]
pub struct PlatformRecordStore {
    pool: SqlitePool,
}

impl std::fmt::Debug for PlatformRecordStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformRecordStore")
            .finish_non_exhaustive()
    }
}

const SELECT_AFTER_SQL: &str = "SELECT seq, recorded_at, record_json, execution, firing FROM \
                                platform_records WHERE run_id = ? AND seq > ? ORDER BY seq";
const SELECT_KIND_SQL: &str = "SELECT seq, recorded_at, record_json, execution, firing FROM \
                               platform_records WHERE run_id = ? AND kind = ? ORDER BY seq";

impl PlatformRecordStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Store a record at the run's next seq, in a transaction of its own.
    pub async fn append(
        &self,
        run_id: &RunId,
        record: &PlatformRecord,
        position: Option<StagePosition>,
    ) -> Result<StoredPlatformRecord> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let stored =
            Self::append_on_connection(&mut transaction, run_id, now_ms(), record, position)
                .await?;
        transaction.commit().await?;
        Ok(stored)
    }

    /// Store a record at the run's next seq on a connection the caller
    /// holds a transaction on.
    pub async fn append_on_connection(
        connection: &mut SqliteConnection,
        run_id: &RunId,
        recorded_at: u64,
        record: &PlatformRecord,
        position: Option<StagePosition>,
    ) -> Result<StoredPlatformRecord> {
        let head: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) FROM platform_records WHERE run_id = ?",
        )
        .bind(run_id.to_string())
        .fetch_one(&mut *connection)
        .await?;
        let seq = u64::try_from(head).unwrap_or(0).saturating_add(1);
        let record_json = serde_json::to_string(record)?;
        sqlx::query(
            "INSERT INTO platform_records (run_id, seq, recorded_at, kind, record_json, \
             execution, firing) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id.to_string())
        .bind(column(seq))
        .bind(column(recorded_at))
        .bind(record.kind().to_string())
        .bind(record_json)
        .bind(position.map(|position| column(position.execution)))
        .bind(position.map(|position| column(position.firing)))
        .execute(&mut *connection)
        .await?;
        Ok(StoredPlatformRecord {
            seq,
            recorded_at,
            record: record.clone(),
            position,
        })
    }

    /// Every record of the run, in seq order.
    pub async fn read(&self, run_id: &RunId) -> Result<Vec<StoredPlatformRecord>> {
        self.read_after(run_id, 0).await
    }

    /// The run's records past `seq`, in seq order.
    pub async fn read_after(&self, run_id: &RunId, seq: u64) -> Result<Vec<StoredPlatformRecord>> {
        let rows = sqlx::query(SELECT_AFTER_SQL)
            .bind(run_id.to_string())
            .bind(column(seq))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(decode_row).collect()
    }

    /// The run's records of one kind, in seq order.
    pub async fn read_kind(
        &self,
        run_id: &RunId,
        kind: PlatformRecordKind,
    ) -> Result<Vec<StoredPlatformRecord>> {
        let rows = sqlx::query(SELECT_KIND_SQL)
            .bind(run_id.to_string())
            .bind(kind.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(decode_row).collect()
    }

    /// The last seq stored for the run, or `None` when it has none.
    pub async fn head(&self, run_id: &RunId) -> Result<Option<u64>> {
        let head: Option<i64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM platform_records WHERE run_id = ?")
                .bind(run_id.to_string())
                .fetch_one(&self.pool)
                .await?;
        Ok(head.and_then(|head| u64::try_from(head).ok()))
    }
}

fn decode_row(row: &SqliteRow) -> Result<StoredPlatformRecord> {
    let seq: i64 = row.try_get("seq")?;
    let recorded_at: i64 = row.try_get("recorded_at")?;
    let record_json: String = row.try_get("record_json")?;
    let execution: Option<i64> = row.try_get("execution")?;
    let firing: Option<i64> = row.try_get("firing")?;
    let record: PlatformRecord = serde_json::from_str(&record_json)?;
    let position = match (execution, firing) {
        (Some(execution), Some(firing)) => Some(StagePosition {
            execution: u64::try_from(execution).unwrap_or(0),
            firing:    u64::try_from(firing).unwrap_or(0),
        }),
        _ => None,
    };
    Ok(StoredPlatformRecord {
        seq: u64::try_from(seq).map_err(|_| Error::InvalidStoredTimestamp {
            record: "platform record",
            field:  "seq",
            value:  seq,
        })?,
        recorded_at: u64::try_from(recorded_at).map_err(|_| Error::InvalidStoredTimestamp {
            record: "platform record",
            field:  "recorded_at",
            value:  recorded_at,
        })?,
        record,
        position,
    })
}

fn column(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use fabro_types::{RunStatus, fixtures, test_support as types_support};
    use serde_json::json;

    use super::*;
    use crate::test_support;

    fn json(records: &[StoredPlatformRecord]) -> serde_json::Value {
        serde_json::to_value(records).expect("stored records serialize")
    }

    fn store() -> PlatformRecordStore {
        PlatformRecordStore::new(test_support::in_memory_pool_with(&[
            fabro_db::PETRI_PROJECTION_MIGRATION_SQL,
        ]))
    }

    fn sample(kind: PlatformRecordKind) -> PlatformRecord {
        match kind {
            PlatformRecordKind::RunCreated => PlatformRecord::RunCreated(RunCreatedRecord {
                spec:         types_support::test_run_spec(),
                title:        Some("A run".to_string()),
                parent_id:    None,
                retried_from: None,
                web_url:      None,
            }),
            PlatformRecordKind::RunLifecycle => PlatformRecord::RunLifecycle(
                RunLifecycleRecord::new(RunLifecycleKind::Running).with_status(RunStatus::Running),
            ),
            PlatformRecordKind::RunTitle => PlatformRecord::RunTitle(RunTitleRecord {
                title: "Renamed".to_string(),
            }),
            PlatformRecordKind::RunParent => PlatformRecord::RunParent(RunParentRecord {
                parent_id:          Some(fixtures::RUN_2),
                previous_parent_id: None,
            }),
            PlatformRecordKind::RunArchived => PlatformRecord::RunArchived,
            PlatformRecordKind::RunUnarchived => PlatformRecord::RunUnarchived,
            PlatformRecordKind::RunSuperseded => {
                PlatformRecord::RunSuperseded(RunSupersededRecord {
                    new_run_id:                fixtures::RUN_2,
                    target_checkpoint_ordinal: 1,
                    target_node_id:            "plan".to_string(),
                    target_visit:              1,
                })
            }
            PlatformRecordKind::RunNotice => PlatformRecord::RunNotice(RunNoticeRecord {
                level:   RunNoticeLevel::Warn,
                code:    "sandbox.slow".to_string(),
                message: "the sandbox took a while".to_string(),
            }),
            PlatformRecordKind::InterviewAnswered => {
                PlatformRecord::InterviewAnswered(InterviewAnsweredRecord {
                    question:  "q-1".to_string(),
                    principal: None,
                    channel:   Some("web".to_string()),
                    text:      None,
                    answer:    None,
                })
            }
            PlatformRecordKind::RunBranch => PlatformRecord::RunBranch(RunBranchRecord {
                run_branch: Some("fabro/run-1".to_string()),
                base_sha:   Some("abc".to_string()),
                workspace:  Some("invocation-0-scope-0".to_string()),
            }),
            PlatformRecordKind::GitIdentity => PlatformRecord::GitIdentity(GitIdentityRecord {
                identity: GitIdentity {
                    name:   "Fabro".to_string(),
                    email:  "fabro@example.com".to_string(),
                    source: fabro_types::GitIdentitySource::Default,
                },
            }),
            PlatformRecordKind::Checkpoint => PlatformRecord::Checkpoint(CheckpointRecord {
                execution:      0,
                firing:         3,
                attempt:        Some(1),
                workspace:      Some("invocation-0-scope-0".to_string()),
                git_commit_sha: Some("def".to_string()),
                diff_summary:   Some(DiffSummary {
                    files_changed: 1,
                    additions:     2,
                    deletions:     0,
                }),
                patch_blob:     None,
                operation:      Some(OperationKey {
                    execution: 0,
                    decision:  DecisionRef::Route {
                        firing:  3,
                        attempt: 1,
                    },
                    effect:    "commit".to_string(),
                }),
            }),
            PlatformRecordKind::ArtifactCollected => {
                PlatformRecord::ArtifactCollected(ArtifactCollectedRecord {
                    execution: 0,
                    firing:    3,
                    attempt:   1,
                    path:      "assets/report.txt".to_string(),
                    blob:      BlobHash::new(b"report"),
                    bytes:     6,
                    digest:    BlobHash::new(b"report").to_string(),
                    operation: Some(OperationKey {
                        execution: 0,
                        decision:  DecisionRef::AttemptStart {
                            firing:  3,
                            attempt: 1,
                        },
                        effect:    "artifact".to_string(),
                    }),
                })
            }
            PlatformRecordKind::RunDiff => PlatformRecord::RunDiff(RunDiffRecord {
                base_sha:     Some("abc".to_string()),
                head_sha:     Some("def".to_string()),
                diff_summary: Some(DiffSummary {
                    files_changed: 1,
                    additions:     2,
                    deletions:     0,
                }),
                patch_blob:   Some(BlobHash::new(b"patch")),
            }),
            PlatformRecordKind::PullRequestCreated => {
                PlatformRecord::PullRequestCreated(PullRequestCreatedRecord {
                    number:    7,
                    owner:     "acme".to_string(),
                    repo:      "widgets".to_string(),
                    html_url:  "https://github.com/acme/widgets/pull/7".to_string(),
                    head_sha:  None,
                    draft:     false,
                    operation: None,
                })
            }
            PlatformRecordKind::PullRequestRequested => {
                PlatformRecord::PullRequestRequested(PullRequestRequestedRecord {
                    creation_id: PullRequestCreationId::new(),
                    model:       "gpt-5.4".to_string(),
                    force:       false,
                })
            }
            PlatformRecordKind::PullRequestFailed => {
                PlatformRecord::PullRequestFailed(PullRequestFailedRecord {
                    creation_id: None,
                    error:       "no remote".to_string(),
                })
            }
            PlatformRecordKind::PullRequestLinked => {
                PlatformRecord::PullRequestLinked(PullRequestLinkedRecord {
                    owner:  "fabro-sh".to_string(),
                    repo:   "fabro".to_string(),
                    number: 7,
                })
            }
            PlatformRecordKind::PullRequestUnlinked => {
                PlatformRecord::PullRequestUnlinked(PullRequestLinkedRecord {
                    owner:  "fabro-sh".to_string(),
                    repo:   "fabro".to_string(),
                    number: 7,
                })
            }
            PlatformRecordKind::NotificationSent => {
                PlatformRecord::NotificationSent(NotificationSentRecord {
                    route:      "slack".to_string(),
                    event:      "run.completed".to_string(),
                    channel:    Some("#runs".to_string()),
                    thread:     None,
                    message_id: None,
                    question:   None,
                    operation:  None,
                })
            }
            PlatformRecordKind::RunPaired => PlatformRecord::RunPaired(RunPairedRecord {
                pair_id: PairId::new(),
                target:  PairTarget {
                    stage_id:   fabro_types::StageId::new("plan", 1),
                    node_label: "Plan".to_string(),
                },
            }),
        }
    }

    #[test]
    fn every_kind_tags_its_record_the_same_way_it_spells_itself() {
        for kind in PlatformRecordKind::VARIANTS {
            let record = sample(*kind);
            assert_eq!(record.kind(), *kind);
            let value = serde_json::to_value(&record).expect("the record serializes");
            assert_eq!(value["kind"], kind.to_string(), "{kind}");
            assert_eq!(
                serde_json::to_value(kind).expect("the kind serializes"),
                json!(kind.to_string())
            );
            let decoded: PlatformRecord =
                serde_json::from_value(value.clone()).expect("the record round-trips");
            assert_eq!(
                serde_json::to_value(&decoded).expect("the decoded record serializes"),
                value
            );
            assert_eq!(
                kind.to_string().parse::<PlatformRecordKind>().ok(),
                Some(*kind)
            );
        }
    }

    #[tokio::test]
    async fn records_get_seqs_per_run_and_read_back_in_order() {
        let store = store();
        let run = fixtures::RUN_1;
        let other = fixtures::RUN_2;
        let first = store
            .append(&run, &sample(PlatformRecordKind::RunCreated), None)
            .await
            .expect("the first record stores");
        let second = store
            .append(
                &run,
                &sample(PlatformRecordKind::Checkpoint),
                Some(StagePosition {
                    execution: 0,
                    firing:    3,
                }),
            )
            .await
            .expect("the second record stores");
        let elsewhere = store
            .append(&other, &sample(PlatformRecordKind::RunArchived), None)
            .await
            .expect("another run's record stores");
        assert_eq!((first.seq, second.seq, elsewhere.seq), (1, 2, 1));

        let stored = store.read(&run).await.expect("the run reads");
        assert_eq!(json(&stored), json(&[first, second.clone()]));
        assert_eq!(
            json(&store.read_after(&run, 1).await.expect("the tail reads")),
            json(std::slice::from_ref(&second))
        );
        assert_eq!(
            json(
                &store
                    .read_kind(&run, PlatformRecordKind::Checkpoint)
                    .await
                    .expect("the kind reads")
            ),
            json(&[second])
        );
        assert_eq!(store.head(&run).await.expect("the head reads"), Some(2));
        assert_eq!(
            store
                .head(&fixtures::RUN_3)
                .await
                .expect("an empty head reads"),
            None
        );
    }
}
