//! Resume on restart: the recovery protocol whose rule is that the
//! workspace a resumed stage sees matches Petri's durable execution state
//! (the integration plan's F3.5).
//!
//! For a Petri run the server finds in flight at startup, once the previous
//! worker's lease is released, [`recover`] reads the durable execution
//! state through `inspect_run` and decides:
//!
//! - a run with a `checkpoint_failed` finish anywhere is reported failed and
//!   not resumed: a failed checkpoint cancelled it, and nothing of it is
//!   reconciled;
//! - otherwise, for every live execution, the last durable finish names the
//!   snapshot its workspace must sit on: the checkpoint record's commit, or,
//!   when the record was lost to the crash, the commit found by its key in the
//!   workspace's snapshot repository or history, which is then recorded again;
//! - a workspace that survives is verified to sit on that commit, unchanged, or
//!   reset to it; a workspace that is gone, or a fresh one with no history (a
//!   fork's first acquisition), is restored from the run's snapshot repository;
//! - a durable finish with no snapshot fails the run with a named error rather
//!   than resume it on stale files.
//!
//! Every child invocation's scope has its own snapshots, keyed by
//! execution; a nested invocation that inherits its caller's sandbox shares
//! the caller's workspace, and the workspace is brought to the newest of
//! the live executions' snapshots on it.
//!
//! The decision is [`plan`], over the records and the snapshot repository
//! alone, both on this host whatever the provider. Applying it differs: a
//! host workspace is brought to its snapshot here, before the worker is
//! relaunched; a Docker or Daytona workspace lives inside a sandbox only
//! the worker's run reaches, so its target is deferred, and the worker's
//! hooks read the same plan and apply it through the scope's environment
//! at `scope_acquired`, before the first attempt runs there
//! ([`bring_to`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use fabro_store::platform_records::CheckpointRecord;
use fabro_store::{PlatformRecord, PlatformRecordKind, StagePosition};
use fabro_types::RunId;
use fabro_types::settings::run::RunNamespace;
use petri_execution::host::{self, HostError};
use petri_execution::inspect::{self, ExecutionInspection, InspectError};
use petri_execution::{Access, InvocationId, RunKey, RunStore};
use petri_store::StoreError;
use tracing::info;

use crate::checkpoint::{
    CHECKPOINT_FAILED_CLASS, CheckpointError, CheckpointKey, RunGitSettings, RunWorkspaces, Site,
};
use crate::platform_records::{PlatformRecordError, PlatformRecords};
use crate::workspace::{WorkspaceLookup, WorkspaceLookupError};

/// What recovery needs: the run, where its workspaces are, its records,
/// and its Git settings.
pub struct RecoveryRequest {
    pub run_id:  RunId,
    /// The run directory Petri ran under (the run's `petri` scratch).
    pub run_dir: PathBuf,
    pub store:   Arc<dyn RunStore>,
    pub records: Arc<dyn PlatformRecords>,
    pub git:     RunGitSettings,
}

impl RecoveryRequest {
    /// The request a run's settings give.
    #[must_use]
    pub fn for_run(
        run_id: RunId,
        run_dir: PathBuf,
        store: Arc<dyn RunStore>,
        records: Arc<dyn PlatformRecords>,
        settings: &RunNamespace,
    ) -> Self {
        Self {
            run_id,
            run_dir,
            store,
            records,
            git: RunGitSettings::from(settings),
        }
    }
}

/// What was done to one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceAction {
    /// It sat on the snapshot, unchanged.
    Verified,
    /// It was brought back to the snapshot.
    Reset,
    /// It was gone and was recreated from the snapshot repository.
    Restored,
    /// It lives in a sandbox this process does not reach: the worker's
    /// hooks bring it to the snapshot when its scope is acquired.
    Deferred,
}

/// The snapshot a workspace must sit on before work resumes in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreTarget {
    pub key: CheckpointKey,
    pub sha: String,
}

/// What recovery decided, before any workspace was touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The store never held the run: it starts from its admitted graphs.
    Start,
    /// The run continues; each live workspace, by id, and its snapshot.
    Resume {
        targets: BTreeMap<String, RestoreTarget>,
    },
    /// The run cannot continue and is reported failed.
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredWorkspace {
    pub workspace: String,
    pub sha:       String,
    pub action:    WorkspaceAction,
}

/// What the server does with the run next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// The store never held the run: it starts from its admitted graphs.
    Start,
    /// The run continues from its records, its live workspaces on their
    /// snapshots.
    Resume { workspaces: Vec<RecoveredWorkspace> },
    /// The run cannot continue and is reported failed.
    Failed { reason: String },
}

/// Why recovery could not decide: the records could not be read, or a
/// workspace could not be brought to its snapshot.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("the run's record could not be opened")]
    Open(#[source] StoreError),
    #[error("the run's coordinator log could not be read")]
    Log(#[source] petri_execution::StoreError),
    #[error("the run's coordinator state could not be read")]
    State(#[source] HostError),
    #[error("the run's record could not be inspected")]
    Inspect(#[source] InspectError),
    #[error("the run's workspaces could not be named")]
    Lookup(#[source] WorkspaceLookupError),
    #[error("the run's checkpoint records could not be read or written")]
    Records(#[source] PlatformRecordError),
    #[error("the workspace `{workspace}` could not be brought to its snapshot")]
    Workspace {
        workspace: String,
        #[source]
        source:    CheckpointError,
    },
}

/// Decide how the run continues: the snapshot every live workspace must sit
/// on, from the records and the snapshot repository, with a lost record
/// reconciled from the repository. Nothing is touched.
pub async fn plan(
    store: Arc<dyn RunStore>,
    records: &dyn PlatformRecords,
    run_id: &RunId,
    workspaces: &RunWorkspaces,
) -> Result<Plan, RecoveryError> {
    let key = RunKey::new(run_id.to_string());
    let logs = match store.open(&key, Access::Read).await {
        Ok(logs) => logs,
        Err(StoreError::NotFound { .. }) => return Ok(Plan::Start),
        Err(error) => return Err(RecoveryError::Open(error)),
    };
    // A record with no root invocation (the worker died between creating
    // the run and declaring it) has nothing to reconcile; the worker's
    // resume starts it again from its admitted graphs.
    let coordinator = petri_execution::read_coordinator_log(&*logs)
        .await
        .map_err(RecoveryError::Log)?;
    if coordinator.is_empty() {
        return Ok(Plan::Resume {
            targets: BTreeMap::new(),
        });
    }
    let state = host::stored_state(&*logs)
        .await
        .map_err(RecoveryError::State)?;
    if !state.invocations.contains_key(&InvocationId::ROOT) {
        return Ok(Plan::Resume {
            targets: BTreeMap::new(),
        });
    }
    let inspection = inspect::inspect_run(&*logs)
        .await
        .map_err(RecoveryError::Inspect)?;
    drop(logs);

    if let Some(failed) = checkpoint_failure(&inspection.executions) {
        return Ok(Plan::Failed { reason: failed });
    }
    let planner = Planner {
        records,
        run_id,
        workspaces,
        lookup: WorkspaceLookup::new(store, key),
        recorded: recorded_checkpoints(records, run_id).await?,
    };
    planner.targets(&inspection.executions).await
}

/// Decide how the run continues, and bring its host workspaces to their
/// snapshots; a sandbox workspace's target is deferred to the worker.
pub async fn recover(request: RecoveryRequest) -> Result<Recovery, RecoveryError> {
    let workspaces = RunWorkspaces::new(
        request.run_dir.clone(),
        request.run_id.to_string(),
        request.git.author.clone(),
        &request.git.checkpoint,
    );
    let targets = match plan(
        Arc::clone(&request.store),
        &*request.records,
        &request.run_id,
        &workspaces,
    )
    .await?
    {
        Plan::Start => return Ok(Recovery::Start),
        Plan::Failed { reason } => return Ok(Recovery::Failed { reason }),
        Plan::Resume { targets } => targets,
    };

    let mut recovered = Vec::new();
    for (workspace, target) in targets {
        let action = if request.git.host_workspaces {
            bring_to(
                &workspaces,
                &workspaces.host(&workspace),
                &workspace,
                &target,
            )
            .await?
        } else {
            WorkspaceAction::Deferred
        };
        info!(
            run_id = %request.run_id,
            workspace,
            sha = target.sha,
            action = ?action,
            "workspace's durable snapshot decided"
        );
        recovered.push(RecoveredWorkspace {
            workspace,
            sha: target.sha,
            action,
        });
    }
    Ok(Recovery::Resume {
        workspaces: recovered,
    })
}

/// The snapshot one live execution's last durable finish names on a
/// workspace.
struct Candidate {
    key: CheckpointKey,
    sha: String,
}

/// The decision over one run's records and snapshot repository.
struct Planner<'a> {
    records:    &'a dyn PlatformRecords,
    run_id:     &'a RunId,
    workspaces: &'a RunWorkspaces,
    lookup:     WorkspaceLookup,
    /// The run's checkpoint records by key: the workspace they name and
    /// the commit.
    recorded:   BTreeMap<CheckpointKey, (Option<String>, String)>,
}

impl Planner<'_> {
    /// The snapshot each live execution's workspace must sit on. A live
    /// execution is one whose log records no exit: `inspect_run` reports
    /// it as incomplete. A workspace several live executions share is
    /// brought to the newest of their snapshots.
    async fn targets(&self, executions: &[ExecutionInspection]) -> Result<Plan, RecoveryError> {
        let mut candidates: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
        for execution in executions
            .iter()
            .filter(|execution| execution.status == "incomplete")
        {
            let Some(key) = last_finish(execution) else {
                continue;
            };
            let owned = self
                .lookup
                .of_invocation(execution.invocation)
                .await
                .map_err(RecoveryError::Lookup)?;
            if owned.is_empty() {
                continue;
            }
            let mut found = false;
            for workspace in owned {
                if let Some(sha) = self.snapshot_of(&workspace, key).await? {
                    found = true;
                    candidates
                        .entry(workspace)
                        .or_default()
                        .push(Candidate { key, sha });
                }
            }
            if !found {
                return Ok(Plan::Failed {
                    reason: format!(
                        "no checkpoint snapshot exists for the last durable finish of {key}; the \
                         run cannot resume on stale files"
                    ),
                });
            }
        }

        let mut targets = BTreeMap::new();
        for (workspace, candidates) in candidates {
            let target = self.newest(&workspace, &candidates).await?;
            targets.insert(workspace, target);
        }
        Ok(Plan::Resume { targets })
    }

    /// The snapshot of `key` in `workspace`: the commit its record names,
    /// when the record names this workspace or none; else the commit found
    /// by its key in the workspace's snapshot repository or history, which
    /// is then recorded again for the record the crash lost. `None` when no
    /// snapshot exists.
    async fn snapshot_of(
        &self,
        workspace: &str,
        key: CheckpointKey,
    ) -> Result<Option<String>, RecoveryError> {
        if let Some((recorded_workspace, sha)) = self.recorded.get(&key) {
            if recorded_workspace
                .as_deref()
                .is_none_or(|recorded| recorded == workspace)
            {
                return Ok(Some(sha.clone()));
            }
        }
        let found = self
            .workspaces
            .find(workspace, key)
            .await
            .map_err(|source| RecoveryError::Workspace {
                workspace: workspace.to_string(),
                source,
            })?;
        if let Some(sha) = &found {
            self.reconcile_record(key, workspace, sha).await?;
        }
        Ok(found)
    }

    /// Write the record a crash lost, from the commit found by its key.
    async fn reconcile_record(
        &self,
        key: CheckpointKey,
        workspace: &str,
        sha: &str,
    ) -> Result<(), RecoveryError> {
        info!(
            run_id = %self.run_id,
            execution = key.execution,
            firing = key.firing,
            attempt = key.attempt,
            sha,
            "checkpoint record reconciled from the run branch"
        );
        let record = PlatformRecord::Checkpoint(CheckpointRecord {
            execution:      key.execution,
            firing:         key.firing,
            attempt:        Some(key.attempt),
            workspace:      Some(workspace.to_string()),
            git_commit_sha: Some(sha.to_string()),
            diff_summary:   None,
            patch_blob:     None,
            operation:      Some(key.operation()),
        });
        self.records
            .append(
                self.run_id,
                &record,
                Some(StagePosition {
                    execution: key.execution,
                    firing:    key.firing,
                }),
            )
            .await
            .map_err(RecoveryError::Records)?;
        Ok(())
    }

    /// Of the snapshots live executions name on one workspace, the one
    /// every other descends from, else the last named. Two executions that
    /// name the same commit share it under the first one's key.
    async fn newest(
        &self,
        workspace: &str,
        candidates: &[Candidate],
    ) -> Result<RestoreTarget, RecoveryError> {
        let mut chosen = &candidates[0];
        for candidate in &candidates[1..] {
            if self
                .workspaces
                .is_ancestor(workspace, &chosen.sha, &candidate.sha)
                .await
                .map_err(|source| RecoveryError::Workspace {
                    workspace: workspace.to_string(),
                    source,
                })?
            {
                chosen = candidate;
            }
        }
        let chosen = candidates
            .iter()
            .find(|candidate| candidate.sha == chosen.sha)
            .unwrap_or(chosen);
        Ok(RestoreTarget {
            key: chosen.key,
            sha: chosen.sha.clone(),
        })
    }
}

/// The reason a run with a failed checkpoint is reported failed, when it
/// has one.
fn checkpoint_failure(executions: &[ExecutionInspection]) -> Option<String> {
    executions.iter().find_map(|execution| {
        execution
            .engine
            .as_ref()?
            .attempts
            .iter()
            .find_map(|attempt| {
                let failure = attempt.failure.as_ref()?;
                (failure.class.as_str() == CHECKPOINT_FAILED_CLASS).then(|| {
                    format!(
                        "the checkpoint of {} (execution {} firing {} attempt {}) failed: {}",
                        attempt.node.as_deref().unwrap_or("a stage"),
                        execution.execution,
                        attempt.firing,
                        attempt.attempt,
                        failure.message
                    )
                })
            })
    })
}

/// The last `StepFinished` of an execution's log, as the key of its
/// snapshot.
fn last_finish(execution: &ExecutionInspection) -> Option<CheckpointKey> {
    let attempt = execution.engine.as_ref()?.attempts.last()?;
    Some(CheckpointKey {
        execution: execution.execution.raw(),
        firing:    attempt.firing,
        attempt:   attempt.attempt,
    })
}

/// The run's checkpoint records by key: the workspace they name and the
/// commit.
async fn recorded_checkpoints(
    records: &dyn PlatformRecords,
    run_id: &RunId,
) -> Result<BTreeMap<CheckpointKey, (Option<String>, String)>, RecoveryError> {
    let stored = records
        .read_kind(run_id, PlatformRecordKind::Checkpoint)
        .await
        .map_err(RecoveryError::Records)?;
    let mut recorded = BTreeMap::new();
    for record in stored {
        let PlatformRecord::Checkpoint(checkpoint) = record.record else {
            continue;
        };
        let key = checkpoint
            .operation
            .as_ref()
            .and_then(CheckpointKey::from_operation);
        if let (Some(key), Some(sha)) = (key, checkpoint.git_commit_sha) {
            recorded.insert(key, (checkpoint.workspace, sha));
        }
    }
    Ok(recorded)
}

/// Verify, reset or restore the workspace at `site` onto its target: a
/// workspace that still holds the commit is verified or reset in place; a
/// gone one, a fresh directory with no history (a fork's first
/// acquisition), or a sandbox whose repository lost the commit, is restored
/// from the snapshot repository (a bundle of the snapshot, into a sandbox).
pub async fn bring_to(
    workspaces: &RunWorkspaces,
    site: &Site,
    workspace: &str,
    target: &RestoreTarget,
) -> Result<WorkspaceAction, RecoveryError> {
    let failed = |source| RecoveryError::Workspace {
        workspace: workspace.to_string(),
        source,
    };
    if workspaces
        .has_commit(site, &target.sha)
        .await
        .map_err(failed)?
    {
        if workspaces
            .matches(site, &target.sha)
            .await
            .map_err(failed)?
        {
            return Ok(WorkspaceAction::Verified);
        }
        workspaces.reset(site, &target.sha).await.map_err(failed)?;
        return Ok(WorkspaceAction::Reset);
    }
    workspaces
        .restore(site, workspace, target.key, &target.sha)
        .await
        .map_err(failed)?;
    Ok(WorkspaceAction::Restored)
}
