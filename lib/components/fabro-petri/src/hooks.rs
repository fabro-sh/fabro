//! Fabro's awaited extension points on a Petri run: the checkpoint commit,
//! its platform record, the artifacts a stage leaves behind, the run's diff,
//! and the run-level ends, wrapped around Petri's own hook service so
//! `[[run.hooks]]` keep running.
//!
//! [`FabroHooks`] implements Petri's `ExecutionHooks` and is installed with
//! `Runtime::hooks` by [`engine::run`](crate::engine::run). It holds the
//! hooks the runtime installed before it (Petri's local hook service behind
//! its adapter, which serves `[[run.hooks]]`) and forwards every point to
//! them, `run_finished` and `scope_released` included, the way Petri's
//! embedding host does. Its own work, at each point:
//!
//! - `prepare_result`: the checkpoint commit, before the `StepFinished` record
//!   is appended, so a durable finish implies a durable snapshot. A stage that
//!   failed on its own terms is committed like a successful one; only a
//!   cancelled attempt is not. A failed commit is fatal to the run: the outcome
//!   becomes a failure of class `checkpoint_failed`, the run is cancelled
//!   through the coordinator handle, and `transition` refuses the firing's
//!   routes, so no route is taken. The commit that creates the run branch also
//!   records where it started: the `run.branch` platform record (the branch
//!   name and the base commit) and the `git.identity` record (who authors the
//!   commits, and where that identity came from), both at that checkpoint's
//!   stage position, so the stream orders them with the firing's finish.
//! - `transition`: the platform checkpoint record, keyed on the Petri position
//!   and the checkpoint's operation identity, with the stage's diff from its
//!   parent commit (`diff_summary`, and the patch as a blob); then the stage's
//!   artifacts: every file under `[run.artifacts] include` in the stage's
//!   workspace goes to configured artifact storage and gets an
//!   `artifact.collected` record, unless the same file with the same content
//!   was already collected earlier in the run. A failed write is a recorded
//!   problem on the transition, never a blocked route.
//! - `run_finished`: the run's diff, its run branch against its base commit, as
//!   the `run.diff` platform record with the patch as a blob; then the
//!   forwarded point, so the local service runs `run_complete` and `run_failed`
//!   with the sandbox in place.
//! - `scope_released`: forwarded, so the local service runs `sandbox_cleanup`
//!   with the sandbox in place. Fabro's own end-of-run work (the terminal
//!   lifecycle event, notifications on it) is the run lifecycle path's, on the
//!   worker's and server's side of the engine, and the workspace's retention is
//!   Petri's, `Retention::Always` for every Fabro setting
//!   ([`engine::RETENTION`](crate::engine::RETENTION)).
//!
//! # Operation identities
//!
//! Every external effect here is keyed on `(run key, execution, DecisionId,
//! effect kind)` from the hook context and deduplicated on retry: the
//! checkpoint's key is the attempt's decision in its execution, effect
//! `checkpoint`; an artifact's is the same decision, effect `artifact`, with
//! the file's path and content digest as the identity within it. A
//! re-dispatched attempt whose commit already landed reuses it when the
//! workspace still sits on it unchanged (see [`RunWorkspaces::commit`]); a
//! reissued routing decision finds the record, or the commit by its
//! trailers, and writes nothing twice; a file already collected under the
//! same path and digest is not collected again.
//!
//! # Where the workspace is
//!
//! On the local provider the commit runs on the host, in the workspace
//! Petri's host backend keeps under the run directory (`crate::checkpoint`).
//! On Docker or Daytona the workspace lives inside the scope's sandbox: the
//! hooks keep the environment Petri hands them at `scope_acquired`, run
//! `git` inside the scope through it, and move the commit out as a bundle
//! into the same snapshot repository the host path pushes to. Artifacts are
//! read out through the same environment on every provider. The same
//! point is where a resumed run brings a workspace to the snapshot its
//! durable state names, before the first attempt runs in it: verified,
//! reset, or, in a fresh sandbox (Petri replaces a lost one on Fabro's
//! request), restored from a bundle of the checkpoint. The plan is
//! [`recovery::plan`], the one the server applied to host workspaces before
//! it relaunched the worker; a host workspace is verified here, unless the
//! run is a fork whose fresh workspace nothing restored yet
//! ([`crate::fork`]), which is restored from the seeded snapshot repository.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fabro_store::platform_records::{
    ArtifactCollectedRecord, CheckpointRecord, GitIdentityRecord, RunBranchRecord, RunDiffRecord,
};
use fabro_store::{PlatformRecord, PlatformRecordKind, StagePosition};
use fabro_types::settings::run::RunNamespace;
use fabro_types::{ArtifactSource, BlobHash, DiffSummary, GitIdentity, RunId};
use fabro_util::error::collect_chain;
use fabro_util::sync;
use fabro_util::workspace_glob::{WorkspaceGlobError, WorkspaceGlobSet};
use petri_execution::{CancelReason, CoordinatorHandle, InvocationId, RunKey, RunStore};
use petri_runtime::driver::lifecycle::{
    AdmitAttempt, AttemptDecision, ExecutionHooks, HookContext, Note, PrepareError, PrepareResult,
    Prepared, Recorded, ResultOrigin, RunFinished, ScopeAcquired, ScopeAcquiredError,
    ScopeReleased, Transition, TransitionError, TransitionReport,
};
use petri_runtime::executor::{EnvError, ExecEnv};
use petri_runtime::ir::{ExecutionId, FailureInfo, ScopeId, Status};
use serde_json::json;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use tokio::{fs, time};
use tracing::{debug, info, warn};

use crate::artifacts::{ArtifactWriteError, ArtifactWriter};
use crate::blobs::Blobs;
use crate::checkpoint::{
    CHECKPOINT_FAILED_CLASS, CheckpointError, CheckpointKey, EXCLUDE_DIRS, RunGitSettings,
    RunWorkspaces, Site, Snapshot, WorkspaceDiff,
};
use crate::platform_records::{PlatformRecordError, PlatformRecords};
use crate::recovery::{self, Plan, RecoveryError, RestoreTarget};
use crate::workspace::{self, WorkspaceLookup, WorkspaceLookupError};

/// The note kind the hooks record on a firing about its checkpoint.
pub const CHECKPOINT_NOTE: &str = "fabro.checkpoint";

/// The effect kind of an artifact collection in its operation identity.
pub const ARTIFACT_EFFECT: &str = "artifact";

/// How often a held checkpoint polls its test gate.
const GATE_POLL: Duration = Duration::from_millis(50);

/// The most files one stage's collection keeps, the legacy executor's
/// budget.
const ARTIFACT_MAX_FILES: usize = 100;
/// The largest file collected, the legacy executor's budget.
const ARTIFACT_MAX_FILE_BYTES: u64 = fabro_types::ARTIFACT_MAX_FILE_BYTES as u64;
/// The most bytes one stage's collection keeps, the legacy executor's
/// budget.
const ARTIFACT_MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
/// How deep a traversal root is listed.
const ARTIFACT_LIST_DEPTH: usize = 64;

/// Why the hooks' own work at a point failed. Each variant keeps its
/// source, and the chain is rendered once, by [`HookError::render`], at the
/// Petri boundaries that carry text: the adjusted outcome of a failed
/// checkpoint, a transition's problems, a scope acquisition's error, and
/// the log.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("the workspace of scope {scope} in invocation {invocation} could not be found")]
    Lookup {
        scope:      ScopeId,
        invocation: InvocationId,
        #[source]
        source:     WorkspaceLookupError,
    },
    #[error("scope {scope} of execution {execution} has no workspace to snapshot")]
    NoWorkspace {
        scope:     ScopeId,
        execution: ExecutionId,
    },
    #[error("the checkpoint commit of `{node}` failed")]
    Commit {
        node:   String,
        #[source]
        source: CheckpointError,
    },
    #[error("the checkpoint commit could not be looked up")]
    Find(#[source] CheckpointError),
    #[error("no checkpoint commit exists for {key}")]
    NoCommit { key: CheckpointKey },
    #[error("the {what} could not be computed")]
    Diff {
        /// `stage's diff` or `run's diff`.
        what:   &'static str,
        #[source]
        source: CheckpointError,
    },
    #[error("the {what} could not be stored")]
    Blob {
        /// `patch`, or the artifact by its path.
        what:   String,
        #[source]
        source: anyhow::Error,
    },
    #[error("the {kind} record could not be written")]
    Write {
        kind:   &'static str,
        #[source]
        source: PlatformRecordError,
    },
    #[error("the run's {kind} records could not be read")]
    Read {
        kind:   &'static str,
        #[source]
        source: PlatformRecordError,
    },
    #[error("invalid run.artifacts.include pattern")]
    Globs(#[source] Arc<WorkspaceGlobError>),
    #[error("the run has no configured artifact writer")]
    NoArtifactWriter,
    #[error("the captured artifact could not be stored")]
    Artifact(#[source] ArtifactWriteError),
    #[error("the workspace could not be listed below `{root}`")]
    List {
        root:   String,
        #[source]
        source: EnvError,
    },
    #[error("the run's restore plan could not be read")]
    Plan(#[source] RecoveryError),
    #[error("{reason}")]
    Unresumable { reason: String },
    #[error(transparent)]
    Restore(RecoveryError),
}

impl HookError {
    /// The error and its causes on one line, for the boundaries that carry
    /// text.
    #[must_use]
    pub fn render(&self) -> String {
        collect_chain(self).join(": ")
    }
}

/// What Fabro's hooks need beside the run: where the platform records go,
/// the run's Git settings, and which files are the run's artifacts.
pub struct HooksSpec {
    pub records:    Arc<dyn PlatformRecords>,
    pub git:        RunGitSettings,
    /// The `[run.artifacts] include` patterns: which files of a stage's
    /// workspace are collected after the stage.
    pub artifacts:  Vec<String>,
    /// A test's gate directory: a checkpoint point named by a `.hold` file
    /// there waits for its `.release` file. `None` outside tests.
    pub test_gates: Option<PathBuf>,
}

impl HooksSpec {
    /// The spec a run's settings give: its Git settings and its artifact
    /// patterns.
    #[must_use]
    pub fn for_run(records: Arc<dyn PlatformRecords>, settings: &RunNamespace) -> Self {
        Self {
            records,
            git: RunGitSettings::from(settings),
            artifacts: settings.artifacts.include.clone(),
            test_gates: None,
        }
    }

    #[must_use]
    pub fn with_test_gates(mut self, gates: Option<PathBuf>) -> Self {
        self.test_gates = gates;
        self
    }
}

/// A scope's sandbox environment as the hooks keep it: the workspace id
/// the executor named, and the environment `git` runs in and files are
/// read through.
type AcquiredEnv = (String, Arc<dyn ExecEnv>);

/// The identity of a collected file: its path and content digest.
type ArtifactIdentity = (String, String);

/// A checkpoint's workspace and commit.
type WorkspaceCommit = (String, String);

/// The run's checkpoints as the hooks track them: what this process
/// committed, what has its platform record, which was recorded last, and
/// the run branch.
#[derive(Default)]
struct CheckpointLedger {
    /// The workspace and commit of every checkpoint this process made.
    committed: Mutex<HashMap<CheckpointKey, WorkspaceCommit>>,
    /// Which checkpoints have their platform record: read from the store
    /// once, then kept current with every append.
    recorded:  OnceCell<Mutex<HashSet<CheckpointKey>>>,
    /// The workspace and commit of the checkpoint recorded last: the head
    /// the run's diff is measured to.
    last:      Mutex<Option<WorkspaceCommit>>,
    /// The run branch as recorded, once: read from the store, or written
    /// by the commit that created the branch.
    branch:    OnceCell<RunBranchRecord>,
}

impl CheckpointLedger {
    fn remember_commit(&self, key: CheckpointKey, workspace: &str, sha: &str) {
        sync::lock(&self.committed).insert(key, (workspace.to_string(), sha.to_string()));
    }

    fn commit_of(&self, key: CheckpointKey) -> Option<WorkspaceCommit> {
        sync::lock(&self.committed).get(&key).cloned()
    }

    fn last(&self) -> Option<WorkspaceCommit> {
        sync::lock(&self.last).clone()
    }

    fn set_last(&self, last: WorkspaceCommit) {
        *sync::lock(&self.last) = Some(last);
    }

    /// The last checkpoint the store named, kept only when this process
    /// has not recorded one yet.
    fn set_last_if_unset(&self, last: Option<WorkspaceCommit>) {
        let mut current = sync::lock(&self.last);
        if current.is_none() {
            *current = last;
        }
    }
}

/// The run's artifacts as the hooks track them: which files are collected,
/// and which were collected already.
struct ArtifactLedger {
    /// The `[run.artifacts] include` patterns, or why they do not parse.
    globs:     Result<WorkspaceGlobSet, Arc<WorkspaceGlobError>>,
    /// Every artifact collected so far, by path and digest: read from the
    /// store once, then kept current with every append.
    collected: OnceCell<Mutex<HashSet<ArtifactIdentity>>>,
}

/// Where each acquired scope's workspace is, and the locks that serialize
/// Git work in it.
#[derive(Default)]
struct ScopeEnvs {
    /// The environment of every acquired scope, by execution and scope,
    /// with the workspace id the executor named: where `git` runs when the
    /// workspaces are not on this host, and where artifacts are read from
    /// on every provider. Dropped at release.
    acquired:  Mutex<HashMap<(ExecutionId, ScopeId), AcquiredEnv>>,
    /// Inherited workspaces resolved through the run's records.
    inherited: Mutex<HashMap<InvocationId, Option<String>>>,
    /// One lock per workspace: the branches of a parallel node and a nested
    /// invocation share their caller's workspace, and Git allows one index
    /// operation at a time in it.
    locks:     Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl ScopeEnvs {
    fn insert(&self, execution: ExecutionId, scope: ScopeId, env: AcquiredEnv) {
        sync::lock(&self.acquired).insert((execution, scope), env);
    }

    fn remove(&self, execution: ExecutionId, scope: ScopeId) {
        sync::lock(&self.acquired).remove(&(execution, scope));
    }

    fn get(&self, execution: ExecutionId, scope: ScopeId) -> Option<AcquiredEnv> {
        sync::lock(&self.acquired).get(&(execution, scope)).cloned()
    }

    /// The inherited workspace of an invocation, once resolved: `None`
    /// when not resolved yet, `Some(None)` when it inherits none.
    #[expect(
        clippy::option_option,
        reason = "the outer option is the cache miss; the inner is an invocation that inherits no workspace"
    )]
    fn inherited(&self, invocation: InvocationId) -> Option<Option<String>> {
        sync::lock(&self.inherited).get(&invocation).cloned()
    }

    fn remember_inherited(&self, invocation: InvocationId, inherited: Option<String>) {
        sync::lock(&self.inherited).insert(invocation, inherited);
    }

    /// The lock that serializes Git work in one workspace.
    fn lock_for(&self, workspace: &str) -> Arc<AsyncMutex<()>> {
        Arc::clone(
            sync::lock(&self.locks)
                .entry(workspace.to_string())
                .or_default(),
        )
    }
}

/// Fabro's `ExecutionHooks`, around the hooks the runtime installed.
pub struct FabroHooks {
    inner:           Arc<dyn ExecutionHooks>,
    run_id:          RunId,
    records:         Arc<dyn PlatformRecords>,
    /// Where diff patches go; `None` records summaries alone.
    blobs:           Option<Arc<dyn Blobs>>,
    artifact_writer: Option<Arc<dyn ArtifactWriter>>,
    workspaces:      RunWorkspaces,
    lookup:          WorkspaceLookup,
    identity:        GitIdentity,
    host_workspaces: bool,
    test_gates:      Option<PathBuf>,
    handle:          OnceLock<CoordinatorHandle>,
    checkpoints:     CheckpointLedger,
    artifacts:       ArtifactLedger,
    scopes:          ScopeEnvs,
    /// The checkpoint failure that ended the run, when one did.
    failure:         Mutex<Option<String>>,
    /// Whether the run continues from its records: a sandbox workspace is
    /// then brought to its snapshot when its scope is first acquired.
    resumed:         bool,
    /// The snapshot every live sandbox workspace must sit on before work
    /// resumes in it, read once from the records; an entry leaves when it
    /// is applied.
    restore:         OnceCell<Mutex<BTreeMap<String, RestoreTarget>>>,
    store:           Arc<dyn RunStore>,
}

impl FabroHooks {
    /// Wrap `inner` (the hooks `Runtime::installed_hooks` returned) for the
    /// run whose records are in `store` under `run_key`, with its
    /// workspaces under `run_dir`. `resumed` says the run continues from
    /// its records, so a sandbox workspace is brought to its snapshot at
    /// its scope's first acquisition. `blobs` holds diff patches;
    /// `artifact_writer` holds captured workspace files.
    #[must_use]
    pub fn new(
        spec: HooksSpec,
        inner: Arc<dyn ExecutionHooks>,
        run_id: RunId,
        run_key: RunKey,
        run_dir: PathBuf,
        store: Arc<dyn RunStore>,
        resumed: bool,
        blobs: Option<Arc<dyn Blobs>>,
        artifact_writer: Option<Arc<dyn ArtifactWriter>>,
    ) -> Self {
        let identity = GitIdentity {
            name:   spec.git.author.name.clone(),
            email:  spec.git.author.email.clone(),
            source: spec.git.identity_source,
        };
        let workspaces = RunWorkspaces::new(
            run_dir,
            run_id.to_string(),
            spec.git.author,
            &spec.git.checkpoint,
        );
        Self {
            inner,
            run_id,
            records: spec.records,
            blobs,
            artifact_writer,
            workspaces,
            lookup: WorkspaceLookup::new(Arc::clone(&store), run_key),
            identity,
            host_workspaces: spec.git.host_workspaces,
            test_gates: spec.test_gates,
            handle: OnceLock::new(),
            checkpoints: CheckpointLedger::default(),
            artifacts: ArtifactLedger {
                globs:     WorkspaceGlobSet::try_new(&spec.artifacts).map_err(Arc::new),
                collected: OnceCell::new(),
            },
            scopes: ScopeEnvs::default(),
            failure: Mutex::default(),
            resumed,
            restore: OnceCell::new(),
            store,
        }
    }

    /// Hand the hooks the running coordinator, so a fatal checkpoint can
    /// cancel the run. Called once, from the host's handle callback.
    pub fn attach(&self, handle: CoordinatorHandle) {
        if self.handle.set(handle).is_err() {
            debug!("the coordinator handle was already attached to the hooks");
        }
    }

    /// The checkpoint failure that ended the run, when one did: what the
    /// engine reports the run failed with.
    #[must_use]
    pub fn checkpoint_failure(&self) -> Option<String> {
        sync::lock(&self.failure).clone()
    }

    /// The run's workspaces on this host, as the hooks reach them.
    #[must_use]
    pub fn workspaces(&self) -> &RunWorkspaces {
        &self.workspaces
    }

    fn fail_run(&self, message: &str) {
        let mut failure = sync::lock(&self.failure);
        if failure.is_none() {
            *failure = Some(message.to_string());
        }
        drop(failure);
        if let Some(handle) = self.handle.get() {
            info!(run_id = %self.run_id, "cancelling the Petri run after a failed checkpoint");
            handle.cancel_root_for(CancelReason::Control);
        } else {
            warn!(
                run_id = %self.run_id,
                "no coordinator handle is attached; the failed checkpoint cannot cancel the run"
            );
        }
    }

    /// The workspace id of `scope` in the context's invocation: the
    /// isolated name when its workspace exists, else the inherited one the
    /// records name, else the isolated name for the caller to report.
    async fn workspace_of(
        &self,
        context: &HookContext,
        scope: ScopeId,
    ) -> Result<String, HookError> {
        let isolated = workspace::isolated_workspace(context.invocation, scope);
        if self.workspaces.workspace_exists(&isolated).await {
            return Ok(isolated);
        }
        let inherited = if let Some(inherited) = self.scopes.inherited(context.invocation) {
            inherited
        } else {
            let inherited = self
                .lookup
                .inherited(context.invocation)
                .await
                .map_err(|source| HookError::Lookup {
                    scope,
                    invocation: context.invocation,
                    source,
                })?;
            self.scopes
                .remember_inherited(context.invocation, inherited.clone());
            inherited
        };
        Ok(inherited.unwrap_or(isolated))
    }

    /// The environment of `scope` in the context's execution, as
    /// `scope_acquired` kept it, with the workspace id the executor named.
    fn env_of(&self, context: &HookContext, scope: ScopeId) -> Option<AcquiredEnv> {
        self.scopes.get(context.execution, scope)
    }

    /// Where `scope`'s workspace is and what to call it: on this host, the
    /// directory the records name (`None` when it does not exist yet); in
    /// a sandbox, the environment kept at `scope_acquired` (`None` before
    /// the scope was acquired).
    async fn site_of(
        &self,
        context: &HookContext,
        scope: ScopeId,
    ) -> Result<Option<(String, Site)>, HookError> {
        if !self.host_workspaces {
            return Ok(self
                .env_of(context, scope)
                .map(|(workspace, env)| (workspace, Site::Sandbox(env))));
        }
        let workspace = self.workspace_of(context, scope).await?;
        if !self.workspaces.workspace_exists(&workspace).await {
            return Ok(None);
        }
        let site = self.workspaces.host(&workspace);
        Ok(Some((workspace, site)))
    }

    /// The checkpoint commit for one attempt's result. `Ok(Some)` is the
    /// note to record, `Ok(None)` nothing to record, `Err` the fatal
    /// failure.
    async fn snapshot(
        &self,
        context: &HookContext,
        scope: ScopeId,
        key: CheckpointKey,
        node: &str,
        status: &Status,
        origin: ResultOrigin,
    ) -> Result<Option<Note>, HookError> {
        let Some((workspace, site)) = self.site_of(context, scope).await? else {
            // A skipped node or a driver-made outcome may precede the scope's
            // environment; nothing of the stage's exists to snapshot.
            if origin == ResultOrigin::Driver || matches!(status, Status::Skipped) {
                return Ok(Some(Note::new(
                    CHECKPOINT_NOTE,
                    json!({
                        "execution": key.execution,
                        "firing": key.firing,
                        "attempt": key.attempt,
                        "skipped": "the scope has no workspace yet",
                    }),
                )));
            }
            return Err(HookError::NoWorkspace {
                scope,
                execution: context.execution,
            });
        };
        self.gate("commit", node).await;
        let serialized = self.scopes.lock_for(&workspace);
        let _held = serialized.lock().await;
        match self
            .workspaces
            .commit(&site, &workspace, key, node, status.tag())
            .await
        {
            Ok(snapshot) => {
                debug!(
                    run_id = %self.run_id,
                    node,
                    execution = key.execution,
                    firing = key.firing,
                    attempt = key.attempt,
                    reused = snapshot.reused,
                    site = ?site,
                    "checkpoint committed"
                );
                self.committed(key, &workspace, &snapshot).await;
                Ok(Some(Note::new(
                    CHECKPOINT_NOTE,
                    json!({
                        "execution": key.execution,
                        "firing": key.firing,
                        "attempt": key.attempt,
                        "workspace": workspace,
                        "git_commit_sha": snapshot.sha,
                        "reused": snapshot.reused,
                    }),
                )))
            }
            Err(source) => Err(HookError::Commit {
                node: node.to_string(),
                source,
            }),
        }
    }

    /// Remember a commit this process made, and record the run branch when
    /// this commit created it.
    async fn committed(&self, key: CheckpointKey, workspace: &str, snapshot: &Snapshot) {
        self.checkpoints
            .remember_commit(key, workspace, &snapshot.sha);
        let Some(branched) = &snapshot.branched else {
            return;
        };
        // A branch that starts from nothing (a workspace with no history) is
        // measured from its first commit: the checkout the run started on.
        let base_sha = branched
            .base_sha
            .clone()
            .unwrap_or_else(|| snapshot.sha.clone());
        if let Err(error) = self.record_branch(key, workspace, base_sha).await {
            warn!(run_id = %self.run_id, error = %error.render(), "the run branch was not recorded");
        }
    }

    /// The `run.branch` and `git.identity` records, once per run: the first
    /// workspace to create the run branch names where it started. A run
    /// that already recorded its branch (a resume, or a nested workspace
    /// after the root's) records nothing. Both records take the position of
    /// the checkpoint that created the branch, so the stream places them
    /// with that firing (after its finish, before its routes) rather than by
    /// the clock, which would put them on either side of the finish from
    /// one run to the next.
    async fn record_branch(
        &self,
        key: CheckpointKey,
        workspace: &str,
        base_sha: String,
    ) -> Result<(), HookError> {
        let position = StagePosition {
            execution: key.execution,
            firing:    key.firing,
        };
        let branch = self
            .checkpoints
            .branch
            .get_or_try_init(|| async {
                if let Some(stored) = self.stored_branch().await? {
                    return Ok::<_, HookError>(stored);
                }
                let record = RunBranchRecord {
                    run_branch: Some(self.workspaces.run_branch()),
                    base_sha:   Some(base_sha.clone()),
                    workspace:  Some(workspace.to_string()),
                };
                self.records
                    .append(
                        &self.run_id,
                        &PlatformRecord::RunBranch(record.clone()),
                        Some(position),
                    )
                    .await
                    .map_err(|source| HookError::Write {
                        kind: "run branch",
                        source,
                    })?;
                let identity = PlatformRecord::GitIdentity(GitIdentityRecord {
                    identity: self.identity.clone(),
                });
                self.records
                    .append(&self.run_id, &identity, Some(position))
                    .await
                    .map_err(|source| HookError::Write {
                        kind: "git identity",
                        source,
                    })?;
                info!(
                    run_id = %self.run_id,
                    workspace,
                    base_sha,
                    "run branch recorded"
                );
                Ok(record)
            })
            .await?;
        debug!(run_id = %self.run_id, base_sha = ?branch.base_sha, "the run branch is recorded");
        Ok(())
    }

    /// The run branch the store already holds, when a record exists.
    async fn stored_branch(&self) -> Result<Option<RunBranchRecord>, HookError> {
        let stored = self
            .records
            .read_kind(&self.run_id, PlatformRecordKind::RunBranch)
            .await
            .map_err(|source| HookError::Read {
                kind: "branch",
                source,
            })?;
        Ok(stored.into_iter().find_map(|stored| match stored.record {
            PlatformRecord::RunBranch(record) => Some(record),
            _ => None,
        }))
    }

    /// The restore plan of a resumed run, read once: what every live
    /// sandbox workspace must be brought to at its first acquisition.
    async fn restore_targets(&self) -> Result<&Mutex<BTreeMap<String, RestoreTarget>>, HookError> {
        self.restore
            .get_or_try_init(|| async {
                let plan = recovery::plan(
                    Arc::clone(&self.store),
                    self.records.as_ref(),
                    &self.run_id,
                    &self.workspaces,
                )
                .await
                .map_err(HookError::Plan)?;
                match plan {
                    Plan::Resume { targets } => Ok(Mutex::new(targets)),
                    Plan::Start => Ok(Mutex::default()),
                    Plan::Failed { reason } => Err(HookError::Unresumable { reason }),
                }
            })
            .await
    }

    /// Bring a workspace to the snapshot the resumed run's durable state
    /// names, once, at its first acquisition. After a restart the server
    /// already brought a host workspace there, so this verifies; a fork's
    /// fresh workspace is restored here from the snapshot repository the
    /// fork seeded; a sandbox workspace is only reachable here.
    async fn restore(&self, workspace: &str, site: &Site) -> Result<(), HookError> {
        let targets = self.restore_targets().await?;
        let target = sync::lock(targets).remove(workspace);
        let Some(target) = target else {
            return Ok(());
        };
        let serialized = self.scopes.lock_for(workspace);
        let _held = serialized.lock().await;
        let action = recovery::bring_to(&self.workspaces, site, workspace, &target)
            .await
            .map_err(HookError::Restore)?;
        info!(
            run_id = %self.run_id,
            workspace,
            sha = target.sha,
            action = ?action,
            site = ?site,
            "workspace brought to its durable snapshot"
        );
        Ok(())
    }

    /// The checkpoint's platform record, once per operation identity, with
    /// the stage's diff from the commit's parent.
    async fn record(
        &self,
        context: &HookContext,
        scope: ScopeId,
        key: CheckpointKey,
    ) -> Result<(), HookError> {
        let recorded = self.recorded_checkpoints().await?;
        if sync::lock(recorded).contains(&key) {
            return Ok(());
        }
        let (workspace, sha) = if let Some(committed) = self.checkpoints.commit_of(key) {
            committed
        } else {
            let acquired = self.env_of(context, scope).map(|(workspace, _)| workspace);
            let workspace = match acquired {
                Some(workspace) => workspace,
                None => self.workspace_of(context, scope).await?,
            };
            let serialized = self.scopes.lock_for(&workspace);
            let held = serialized.lock().await;
            let found = self.workspaces.find(&workspace, key).await;
            drop(held);
            let sha = found
                .map_err(HookError::Find)?
                .ok_or(HookError::NoCommit { key })?;
            (workspace, sha)
        };
        let (diff_summary, patch_blob) = match self.stage_diff(&workspace, &sha).await {
            Ok(diff) => diff,
            Err(error) => {
                // The record still names the commit; the diff is a view.
                warn!(
                    run_id = %self.run_id,
                    workspace,
                    sha,
                    error = %error.render(),
                    "the checkpoint's diff was not computed"
                );
                (None, None)
            }
        };
        let record = PlatformRecord::Checkpoint(CheckpointRecord {
            execution: key.execution,
            firing: key.firing,
            attempt: Some(key.attempt),
            workspace: Some(workspace.clone()),
            git_commit_sha: Some(sha.clone()),
            diff_summary,
            patch_blob,
            operation: Some(key.operation()),
        });
        self.records
            .append(
                &self.run_id,
                &record,
                Some(StagePosition {
                    execution: key.execution,
                    firing:    key.firing,
                }),
            )
            .await
            .map_err(|source| HookError::Write {
                kind: "checkpoint",
                source,
            })?;
        sync::lock(recorded).insert(key);
        self.checkpoints.set_last((workspace, sha));
        Ok(())
    }

    /// A stage's diff: its checkpoint commit against the commit's parent.
    /// A root commit (the first snapshot of a workspace with no history)
    /// has none. The patch goes to the blob table when the run has one and
    /// the diff is not empty.
    async fn stage_diff(
        &self,
        workspace: &str,
        sha: &str,
    ) -> Result<(Option<DiffSummary>, Option<BlobHash>), HookError> {
        let failed = |source| HookError::Diff {
            what: "stage's diff",
            source,
        };
        let parent = self
            .workspaces
            .commit_parent(workspace, sha)
            .await
            .map_err(failed)?;
        let Some(parent) = parent else {
            return Ok((None, None));
        };
        let diff = self
            .workspaces
            .diff(workspace, Some(&parent), sha)
            .await
            .map_err(failed)?;
        let patch_blob = self.patch_blob(&diff).await?;
        Ok((Some(diff.summary), patch_blob))
    }

    /// The patch of a diff in the blob table, when the diff is not empty
    /// and the run has a blob table.
    async fn patch_blob(&self, diff: &WorkspaceDiff) -> Result<Option<BlobHash>, HookError> {
        if diff.is_empty() {
            return Ok(None);
        }
        let Some(blobs) = &self.blobs else {
            return Ok(None);
        };
        blobs
            .write(diff.patch.as_bytes())
            .await
            .map(Some)
            .map_err(|source| HookError::Blob {
                what: "patch".to_string(),
                source,
            })
    }

    /// The checkpoints already recorded for the run, read once: what a
    /// resume's reissued routing decisions must not record again, and
    /// where the run's diff is measured to when this process made no
    /// checkpoint yet.
    async fn recorded_checkpoints(&self) -> Result<&Mutex<HashSet<CheckpointKey>>, HookError> {
        self.checkpoints
            .recorded
            .get_or_try_init(|| async {
                let stored = self
                    .records
                    .read_kind(&self.run_id, PlatformRecordKind::Checkpoint)
                    .await
                    .map_err(|source| HookError::Read {
                        kind: "checkpoint",
                        source,
                    })?;
                let mut recorded = HashSet::new();
                let mut last = None;
                for record in stored {
                    let PlatformRecord::Checkpoint(checkpoint) = &record.record else {
                        continue;
                    };
                    if let Some(key) = checkpoint
                        .operation
                        .as_ref()
                        .and_then(CheckpointKey::from_operation)
                    {
                        recorded.insert(key);
                    }
                    if let (Some(workspace), Some(sha)) =
                        (&checkpoint.workspace, &checkpoint.git_commit_sha)
                    {
                        last = Some((workspace.clone(), sha.clone()));
                    }
                }
                self.checkpoints.set_last_if_unset(last);
                Ok(Mutex::new(recorded))
            })
            .await
    }

    /// The artifacts of a finished attempt: every file of its workspace
    /// under the run's patterns, stored once. `Ok` is how many files were
    /// collected; `Err` names the first problem that stopped the
    /// collection.
    async fn collect_artifacts(
        &self,
        context: &HookContext,
        scope: ScopeId,
        key: CheckpointKey,
    ) -> Result<usize, HookError> {
        let globs = match &self.artifacts.globs {
            Ok(globs) => globs,
            Err(error) => return Err(HookError::Globs(Arc::clone(error))),
        };
        if globs.is_empty() {
            return Ok(0);
        }
        let Some((_, env)) = self.env_of(context, scope) else {
            // A skipped node or a driver-made outcome may precede the scope's
            // environment; there is no workspace to collect from.
            return Ok(0);
        };
        let candidates = list_artifacts(env.as_ref(), globs).await?;
        let limit = usize::try_from(ARTIFACT_MAX_FILE_BYTES).unwrap_or(usize::MAX);
        let mut collected = 0;
        let mut total_bytes = 0_u64;
        for (path, size) in select_artifacts(candidates) {
            if total_bytes.saturating_add(size) > ARTIFACT_MAX_TOTAL_BYTES {
                break;
            }
            let bytes = match env.read_file_limited(Path::new(&path), limit).await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(error) => {
                    warn!(run_id = %self.run_id, path, error = %error, "an artifact could not be read");
                    continue;
                }
            };
            if !self.store_artifact(key, path, &bytes).await? {
                continue;
            }
            total_bytes = total_bytes.saturating_add(size);
            collected += 1;
        }
        Ok(collected)
    }

    /// Publish bytes before their record; only a recorded capture enters
    /// the ledger. Failed writes remain retryable, including after restart.
    async fn store_artifact(
        &self,
        key: CheckpointKey,
        path: String,
        bytes: &[u8],
    ) -> Result<bool, HookError> {
        let already = self.collected_artifacts().await?;
        let digest = BlobHash::new(bytes);
        let identity = (path.clone(), digest.to_string());
        if sync::lock(already).contains(&identity) {
            return Ok(false);
        }
        let writer = self
            .artifact_writer
            .as_ref()
            .ok_or(HookError::NoArtifactWriter)?;
        let stored = writer.write(bytes).await.map_err(HookError::Artifact)?;
        if stored != digest {
            return Err(HookError::Artifact(ArtifactWriteError::Integrity {
                expected: digest,
                actual:   stored,
            }));
        }
        let record = PlatformRecord::ArtifactCollected(ArtifactCollectedRecord {
            execution: key.execution,
            firing:    key.firing,
            attempt:   key.attempt,
            path:      path.clone(),
            source:    ArtifactSource::ObjectStore(stored),
            bytes:     u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            digest:    digest.to_string(),
            operation: Some(key.operation_for(ARTIFACT_EFFECT)),
        });
        self.records
            .append(
                &self.run_id,
                &record,
                Some(StagePosition {
                    execution: key.execution,
                    firing:    key.firing,
                }),
            )
            .await
            .map_err(|source| HookError::Write {
                kind: "artifact",
                source,
            })?;
        sync::lock(already).insert(identity);
        Ok(true)
    }

    /// The artifacts already collected for the run, read once: a file that
    /// is unchanged since it was collected is not collected again.
    async fn collected_artifacts(&self) -> Result<&Mutex<HashSet<ArtifactIdentity>>, HookError> {
        self.artifacts
            .collected
            .get_or_try_init(|| async {
                let stored = self
                    .records
                    .read_kind(&self.run_id, PlatformRecordKind::ArtifactCollected)
                    .await
                    .map_err(|source| HookError::Read {
                        kind: "artifact",
                        source,
                    })?;
                let collected = stored
                    .into_iter()
                    .filter_map(|record| match record.record {
                        PlatformRecord::ArtifactCollected(artifact) => {
                            Some((artifact.path, artifact.digest))
                        }
                        _ => None,
                    })
                    .collect();
                Ok(Mutex::new(collected))
            })
            .await
    }

    /// The run's diff: the run branch's last checkpoint against the base
    /// the branch started from, in the snapshot repository on this host.
    /// Nothing is recorded for a run that never created its branch or
    /// never checkpointed.
    async fn record_run_diff(&self) -> Result<(), HookError> {
        self.recorded_checkpoints().await?;
        let branch = match self.checkpoints.branch.get() {
            Some(branch) => Some(branch.clone()),
            None => self.stored_branch().await?,
        };
        let Some(branch) = branch else {
            debug!(run_id = %self.run_id, "no run branch is recorded; no run diff");
            return Ok(());
        };
        let Some(base_sha) = branch.base_sha.clone() else {
            return Ok(());
        };
        let Some((workspace, head_sha)) = self.checkpoints.last() else {
            debug!(run_id = %self.run_id, "no checkpoint is recorded; no run diff");
            return Ok(());
        };
        // The run's diff is measured in the workspace the branch started
        // in; a last checkpoint elsewhere (a nested invocation's workspace)
        // is not this branch's head.
        let workspace = branch.workspace.clone().unwrap_or(workspace);
        let diff = self
            .workspaces
            .diff(&workspace, Some(&base_sha), &head_sha)
            .await
            .map_err(|source| HookError::Diff {
                what: "run's diff",
                source,
            })?;
        let patch_blob = self.patch_blob(&diff).await?;
        let record = PlatformRecord::RunDiff(RunDiffRecord {
            base_sha: Some(base_sha),
            head_sha: Some(head_sha),
            diff_summary: Some(diff.summary),
            patch_blob,
        });
        self.records
            .append(&self.run_id, &record, None)
            .await
            .map_err(|source| HookError::Write {
                kind: "run diff",
                source,
            })?;
        info!(
            run_id = %self.run_id,
            files_changed = diff.summary.files_changed,
            additions = diff.summary.additions,
            deletions = diff.summary.deletions,
            "run diff recorded"
        );
        Ok(())
    }

    /// Hold at a test gate when one is set for this point and node.
    async fn gate(&self, point: &str, node: &str) {
        let Some(dir) = &self.test_gates else {
            return;
        };
        let hold = dir.join(format!("{point}.{node}.hold"));
        if !fs::try_exists(&hold).await.unwrap_or(false) {
            return;
        }
        let release = dir.join(format!("{point}.{node}.release"));
        info!(point, node, "checkpoint held at a test gate");
        while !fs::try_exists(&release).await.unwrap_or(false) {
            time::sleep(GATE_POLL).await;
        }
        info!(point, node, "checkpoint released by its test gate");
    }
}

/// Every file under the patterns' traversal roots that matches a pattern,
/// with its size, listed through the scope's environment. Directories
/// never committed are never collected either.
async fn list_artifacts(
    env: &dyn ExecEnv,
    globs: &WorkspaceGlobSet,
) -> Result<Vec<(String, u64)>, HookError> {
    let mut files = Vec::new();
    for root in globs.traversal_roots() {
        let listed = env
            .list_directory(
                Path::new(if root.is_empty() { "." } else { root }),
                ARTIFACT_LIST_DEPTH,
            )
            .await
            .map_err(|source| HookError::List {
                root: root.to_string(),
                source,
            })?;
        for entry in listed {
            if entry.is_dir {
                continue;
            }
            let path = entry.path.trim_start_matches("./").to_string();
            let path = if root.is_empty() || path.starts_with(&format!("{root}/")) {
                path
            } else {
                format!("{root}/{path}")
            };
            if path
                .split('/')
                .any(|segment| EXCLUDE_DIRS.contains(&segment))
            {
                continue;
            }
            if !globs.is_match(&path) {
                continue;
            }
            files.push((path, entry.size.unwrap_or(0)));
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// The files within the collection's budget: the legacy executor's rule,
/// smallest first, each under the file limit, at most the count limit.
fn select_artifacts(mut candidates: Vec<(String, u64)>) -> Vec<(String, u64)> {
    candidates.retain(|(_, size)| *size <= ARTIFACT_MAX_FILE_BYTES);
    candidates.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    let mut total = 0_u64;
    let mut selected = Vec::new();
    for (path, size) in candidates {
        if selected.len() >= ARTIFACT_MAX_FILES
            || total.saturating_add(size) > ARTIFACT_MAX_TOTAL_BYTES
        {
            break;
        }
        total = total.saturating_add(size);
        selected.push((path, size));
    }
    selected
}

fn is_checkpoint_failure(status: &Status) -> bool {
    matches!(status, Status::Failure(info) if info.class.as_str() == CHECKPOINT_FAILED_CLASS)
}

#[async_trait::async_trait]
impl ExecutionHooks for FabroHooks {
    async fn before_attempt(
        &self,
        context: &HookContext,
        request: AdmitAttempt,
    ) -> AttemptDecision {
        self.inner.before_attempt(context, request).await
    }

    async fn prepare_result(
        &self,
        context: &HookContext,
        request: PrepareResult,
    ) -> Result<Prepared, PrepareError> {
        let node = request.view.node_name().to_owned();
        let scope = request.view.scope;
        let key = CheckpointKey {
            execution: context.execution.raw(),
            firing:    request.view.firing.raw(),
            attempt:   request.view.attempt.raw(),
        };
        let original = request.outcome.status.clone();
        let origin = request.origin;
        let mut prepared = self.inner.prepare_result(context, request).await?;
        let effective = prepared.adjustment.status.clone().unwrap_or(original);
        if matches!(effective, Status::Cancelled) {
            return Ok(prepared);
        }
        match self
            .snapshot(context, scope, key, &node, &effective, origin)
            .await
        {
            Ok(Some(note)) => prepared.notes.push(note),
            Ok(None) => {}
            Err(error) => {
                let message = error.render();
                warn!(
                    run_id = %self.run_id,
                    node,
                    execution = key.execution,
                    firing = key.firing,
                    attempt = key.attempt,
                    error = %message,
                    "checkpoint failed; the run ends"
                );
                self.fail_run(&message);
                prepared.adjustment.status = Some(Status::Failure(
                    FailureInfo::new(message.clone()).with_class(CHECKPOINT_FAILED_CLASS),
                ));
                prepared.adjustment.reason = Some(message);
            }
        }
        Ok(prepared)
    }

    async fn after_record(&self, context: &HookContext, recorded: Recorded) -> Vec<Note> {
        self.inner.after_record(context, recorded).await
    }

    async fn transition(
        &self,
        context: &HookContext,
        transition: Transition,
    ) -> Result<TransitionReport, TransitionError> {
        if is_checkpoint_failure(&transition.outcome.status) {
            return Err(TransitionError::new(
                "the stage's checkpoint commit failed; no route is taken",
            ));
        }
        let node = transition.view.node_name().to_owned();
        let scope = transition.view.scope;
        let key = CheckpointKey {
            execution: context.execution.raw(),
            firing:    transition.view.firing.raw(),
            attempt:   transition.view.attempt.raw(),
        };
        let mut problems = Vec::new();
        self.gate("record", &node).await;
        if let Err(problem) = self.record(context, scope, key).await {
            warn!(
                run_id = %self.run_id,
                node,
                execution = key.execution,
                firing = key.firing,
                error = %problem,
                "the checkpoint record was not written"
            );
            problems.push(problem.render());
        }
        match self.collect_artifacts(context, scope, key).await {
            Ok(0) => {}
            Ok(collected) => {
                debug!(
                    run_id = %self.run_id,
                    node,
                    execution = key.execution,
                    firing = key.firing,
                    collected,
                    "artifacts collected"
                );
            }
            Err(problem) => {
                warn!(
                    run_id = %self.run_id,
                    node,
                    execution = key.execution,
                    firing = key.firing,
                    error = %problem,
                    "artifact collection failed"
                );
                problems.push(format!("artifact collection failed: {}", problem.render()));
            }
        }
        let mut report = self.inner.transition(context, transition).await?;
        report.problems.extend(problems);
        Ok(report)
    }

    async fn run_finished(&self, context: &HookContext, finished: RunFinished) -> Vec<Note> {
        info!(
            run_id = %self.run_id,
            status = ?finished.status,
            failure = finished.failure.as_deref().unwrap_or(""),
            "Petri run finished; recording the run's diff and running the run-end hooks"
        );
        if let Err(error) = self.record_run_diff().await {
            warn!(run_id = %self.run_id, error = %error.render(), "the run's diff was not recorded");
        }
        self.inner.run_finished(context, finished).await
    }

    async fn scope_released(&self, context: &HookContext, released: ScopeReleased) -> Vec<Note> {
        debug!(
            run_id = %self.run_id,
            scope = %released.scope,
            outcome = ?released.outcome,
            "scope released; running the sandbox cleanup hooks"
        );
        let scope = released.scope;
        let notes = self.inner.scope_released(context, released).await;
        self.scopes.remove(context.execution, scope);
        notes
    }

    async fn scope_acquired(
        &self,
        context: &HookContext,
        acquired: ScopeAcquired,
    ) -> Result<(), ScopeAcquiredError> {
        self.inner.scope_acquired(context, acquired.clone()).await?;
        let workspace = acquired.workspace.as_str().to_owned();
        self.scopes.insert(
            context.execution,
            acquired.scope,
            (workspace.clone(), Arc::clone(&acquired.env)),
        );
        if !self.resumed {
            return Ok(());
        }
        let site = if self.host_workspaces {
            self.workspaces.host(&workspace)
        } else {
            Site::Sandbox(Arc::clone(&acquired.env))
        };
        self.restore(&workspace, &site)
            .await
            .map_err(|error| ScopeAcquiredError::new(error.render()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use fabro_store::{ArtifactStore, StoredPlatformRecord};
    use object_store::memory::InMemory;

    use super::*;
    use crate::artifacts::StoreArtifactWriter;
    use crate::test_support::MemoryPlatformRecords;

    struct NoHooks;
    #[async_trait::async_trait]
    impl ExecutionHooks for NoHooks {}

    struct FlakyRecords {
        records: MemoryPlatformRecords,
        fail:    AtomicBool,
    }

    #[async_trait::async_trait]
    impl PlatformRecords for FlakyRecords {
        async fn append(
            &self,
            run_id: &RunId,
            record: &PlatformRecord,
            position: Option<StagePosition>,
        ) -> Result<StoredPlatformRecord, PlatformRecordError> {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(PlatformRecordError::Store(fabro_store::Error::Io(
                    std::io::Error::other("test append unavailable"),
                )));
            }
            self.records.append(run_id, record, position).await
        }
        async fn read_kind(
            &self,
            run_id: &RunId,
            kind: PlatformRecordKind,
        ) -> Result<Vec<StoredPlatformRecord>, PlatformRecordError> {
            self.records.read_kind(run_id, kind).await
        }
    }

    struct FlakyWriter {
        writer: StoreArtifactWriter,
        fail:   AtomicBool,
    }

    #[async_trait::async_trait]
    impl ArtifactWriter for FlakyWriter {
        async fn write(&self, bytes: &[u8]) -> Result<BlobHash, ArtifactWriteError> {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(ArtifactWriteError::Store(fabro_store::Error::Io(
                    std::io::Error::other("test store unavailable"),
                )));
            }
            self.writer.write(bytes).await
        }
    }

    fn artifact_hooks(
        root: &Path,
        run_id: RunId,
        records: Arc<dyn PlatformRecords>,
        writer: Option<Arc<dyn ArtifactWriter>>,
    ) -> FabroHooks {
        FabroHooks::new(
            HooksSpec {
                records,
                git: RunGitSettings::default(),
                artifacts: vec!["assets/**".to_string()],
                test_gates: None,
            },
            Arc::new(NoHooks),
            run_id,
            RunKey::new(run_id.to_string()),
            root.to_path_buf(),
            Arc::new(petri_store::MemoryRunStore::new()),
            false,
            None,
            writer,
        )
    }

    #[tokio::test]
    async fn artifact_failures_preserve_sources_and_retry_without_false_ledger_entries() {
        for fail_upload in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let run = RunId::new();
            let store = ArtifactStore::new(Arc::new(InMemory::new()), "artifacts");
            let records = Arc::new(FlakyRecords {
                records: MemoryPlatformRecords::new(),
                fail:    AtomicBool::new(!fail_upload),
            });
            let writer = Arc::new(FlakyWriter {
                writer: StoreArtifactWriter::new(store.clone(), run),
                fail:   AtomicBool::new(fail_upload),
            });
            let hooks = artifact_hooks(root.path(), run, records.clone(), Some(writer.clone()));
            let key = CheckpointKey {
                execution: 0,
                firing:    1,
                attempt:   1,
            };
            let path = "assets/report.bin".to_string();
            let bytes = b"binary\0payload";
            let hash = BlobHash::new(bytes);
            let error = hooks
                .store_artifact(key, path.clone(), bytes)
                .await
                .unwrap_err();
            assert!(error.render().contains(if fail_upload {
                "test store unavailable"
            } else {
                "test append unavailable"
            }));
            assert!(records.records.records(&run).is_empty());
            assert!(sync::lock(hooks.collected_artifacts().await.unwrap()).is_empty());
            assert_eq!(
                store.get_capture(&run, &hash).await.unwrap().is_some(),
                !fail_upload
            );
            assert!(store.list_for_run(&run).await.unwrap().is_empty());
            assert!(
                hooks
                    .store_artifact(key, path.clone(), bytes)
                    .await
                    .unwrap()
            );
            assert!(
                !hooks
                    .store_artifact(key, path.clone(), bytes)
                    .await
                    .unwrap()
            );
            let resumed = artifact_hooks(root.path(), run, records.clone(), Some(writer));
            assert!(
                !resumed
                    .store_artifact(key, path.clone(), bytes)
                    .await
                    .unwrap()
            );
            assert!(resumed.store_artifact(key, path, b"changed").await.unwrap());
            assert_eq!(records.records.records(&run).len(), 2);
        }
    }

    #[tokio::test]
    async fn artifact_legacy_resume_preserves_sources_and_new_run_isolation() {
        let root = tempfile::tempdir().unwrap();
        let run = RunId::new();
        let records = Arc::new(MemoryPlatformRecords::new());
        let store = ArtifactStore::new(Arc::new(InMemory::new()), "artifacts");
        let key = CheckpointKey {
            execution: 0,
            firing:    1,
            attempt:   1,
        };
        let bytes = b"old payload";
        let hash = BlobHash::new(bytes);
        let path = "assets/report.bin".to_string();
        records
            .append(
                &run,
                &PlatformRecord::ArtifactCollected(ArtifactCollectedRecord {
                    execution: key.execution,
                    firing:    key.firing,
                    attempt:   key.attempt,
                    path:      path.clone(),
                    source:    ArtifactSource::SqliteBlob(hash),
                    bytes:     bytes.len() as u64,
                    digest:    hash.to_string(),
                    operation: None,
                }),
                None,
            )
            .await
            .unwrap();
        let resumed = artifact_hooks(
            root.path(),
            run,
            records.clone(),
            Some(Arc::new(StoreArtifactWriter::new(store.clone(), run))),
        );
        assert!(
            !resumed
                .store_artifact(key, path.clone(), bytes)
                .await
                .unwrap()
        );
        assert!(store.get_capture(&run, &hash).await.unwrap().is_none());
        assert!(
            resumed
                .store_artifact(key, path.clone(), b"changed")
                .await
                .unwrap()
        );
        assert_eq!(records.records(&run).len(), 2);

        // A fork has its own run ID and no inherited capture records. The same
        // payload must be captured under that run, without a cross-run reference.
        let fork = RunId::new();
        let fork_hooks = artifact_hooks(
            root.path(),
            fork,
            records.clone(),
            Some(Arc::new(StoreArtifactWriter::new(store.clone(), fork))),
        );
        assert!(fork_hooks.store_artifact(key, path, bytes).await.unwrap());
        assert_eq!(
            store.get_capture(&fork, &hash).await.unwrap().unwrap(),
            bytes.as_slice()
        );
        assert!(store.get_capture(&run, &hash).await.unwrap().is_none());
        assert_eq!(records.records(&fork).len(), 1);
    }

    #[tokio::test]
    async fn artifact_capture_requires_its_writer_without_falling_back_to_blobs() {
        let root = tempfile::tempdir().unwrap();
        let run = RunId::new();
        let records = Arc::new(MemoryPlatformRecords::new());
        let hooks = artifact_hooks(root.path(), run, records.clone(), None);
        let key = CheckpointKey {
            execution: 0,
            firing:    1,
            attempt:   1,
        };
        assert!(matches!(
            hooks
                .store_artifact(key, "assets/file".to_string(), b"payload")
                .await,
            Err(HookError::NoArtifactWriter)
        ));
        assert!(records.records(&run).is_empty());
    }

    #[test]
    fn the_selection_keeps_the_smallest_files_within_the_budgets() {
        let mut candidates: Vec<(String, u64)> = (0..(ARTIFACT_MAX_FILES + 5))
            .map(|index| (format!("file{index:03}.txt"), 100))
            .collect();
        candidates.push(("huge.bin".to_string(), ARTIFACT_MAX_FILE_BYTES + 1));
        candidates.push(("tiny.txt".to_string(), 1));
        let selected = select_artifacts(candidates);
        assert_eq!(selected.len(), ARTIFACT_MAX_FILES);
        assert_eq!(selected[0], ("tiny.txt".to_string(), 1));
        assert!(selected.iter().all(|(path, _)| path != "huge.bin"));
    }
}
