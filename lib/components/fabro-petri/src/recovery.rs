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
//! ([`bring_sandbox_to`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use fabro_checkpoint::author::GitAuthor;
use fabro_store::platform_records::CheckpointRecord;
use fabro_store::{PlatformRecord, PlatformRecordKind, StagePosition};
use fabro_types::settings::run::{RunCheckpointSettings, RunNamespace};
use fabro_types::{RunId, SandboxProviderKind};
use petri_execution::host::{self, HostError};
use petri_execution::inspect::{self, ExecutionInspection, InspectError};
use petri_execution::{Access, InvocationId, RunKey, RunStore};
use petri_runtime::executor::ExecEnv;
use petri_store::StoreError;
use tracing::info;

use crate::checkpoint::{CHECKPOINT_FAILED_CLASS, CheckpointError, CheckpointKey, RunWorkspaces};
use crate::platform_records::{PlatformRecordError, PlatformRecords};
use crate::workspace::{WorkspaceLookup, WorkspaceLookupError};

/// What recovery needs: the run, where its workspaces are, its records.
pub struct RecoveryRequest {
    pub run_id:          RunId,
    /// The run directory Petri ran under (the run's `petri` scratch).
    pub run_dir:         PathBuf,
    pub store:           Arc<dyn RunStore>,
    pub records:         Arc<dyn PlatformRecords>,
    pub author:          GitAuthor,
    pub checkpoint:      RunCheckpointSettings,
    /// Whether the run's workspaces are on this host.
    pub host_workspaces: bool,
}

impl RecoveryRequest {
    /// The request a run's settings give: its Git author, its checkpoint
    /// settings, and whether its sandbox provider keeps workspaces on this
    /// host.
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
            author: settings
                .git
                .author
                .as_ref()
                .map(GitAuthor::from)
                .unwrap_or_default(),
            checkpoint: settings.checkpoint.clone(),
            host_workspaces: settings.environment.provider == SandboxProviderKind::LOCAL,
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

/// One live execution's last durable finish and the snapshot it names.
struct Target {
    execution: u64,
    key:       CheckpointKey,
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
    // resume reports it as such.
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

    let lookup = WorkspaceLookup::new(Arc::clone(&store), key);
    let recorded = recorded_checkpoints(records, run_id).await?;

    // The snapshot each live execution's workspace must sit on. A live
    // execution is one whose log records no exit: `inspect_run` reports it
    // as incomplete.
    let mut candidates: BTreeMap<String, Vec<(Target, String)>> = BTreeMap::new();
    for execution in inspection
        .executions
        .iter()
        .filter(|execution| execution.status == "incomplete")
    {
        let Some(target) = last_finish(execution) else {
            continue;
        };
        let owned = lookup
            .of_invocation(execution.invocation)
            .await
            .map_err(RecoveryError::Lookup)?;
        if owned.is_empty() {
            continue;
        }
        let mut found = false;
        for workspace in owned {
            let sha = match recorded.get(&target.key) {
                Some((recorded_workspace, sha))
                    if recorded_workspace.as_deref().is_none_or(|w| w == workspace) =>
                {
                    Some(sha.clone())
                }
                _ => {
                    let sha = workspaces
                        .find(&workspace, target.key)
                        .await
                        .map_err(|source| RecoveryError::Workspace {
                            workspace: workspace.clone(),
                            source,
                        })?;
                    if let Some(sha) = &sha {
                        reconcile_record(records, run_id, target.key, &workspace, sha).await?;
                    }
                    sha
                }
            };
            if let Some(sha) = sha {
                found = true;
                candidates
                    .entry(workspace)
                    .or_default()
                    .push((Target { ..target }, sha));
            }
        }
        if !found {
            return Ok(Plan::Failed {
                reason: format!(
                    "no checkpoint snapshot exists for the last durable finish of execution {} \
                     (firing {} attempt {}); the run cannot resume on stale files",
                    target.execution, target.key.firing, target.key.attempt
                ),
            });
        }
    }

    let mut targets = BTreeMap::new();
    for (workspace, candidates) in candidates {
        let sha = newest(workspaces, &workspace, &candidates).await?;
        let key = candidates
            .iter()
            .find(|(_, candidate)| *candidate == sha)
            .map_or(candidates[0].0.key, |(target, _)| target.key);
        targets.insert(workspace, RestoreTarget { key, sha });
    }
    Ok(Plan::Resume { targets })
}

/// Decide how the run continues, and bring its host workspaces to their
/// snapshots; a sandbox workspace's target is deferred to the worker.
pub async fn recover(request: RecoveryRequest) -> Result<Recovery, RecoveryError> {
    let workspaces = RunWorkspaces::new(
        request.run_dir.clone(),
        request.run_id.to_string(),
        request.author.clone(),
        &request.checkpoint,
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
        let action = if request.host_workspaces {
            bring_host_to(&workspaces, &workspace, &target).await?
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

/// The last `StepFinished` of an execution's log.
fn last_finish(execution: &ExecutionInspection) -> Option<Target> {
    let attempt = execution.engine.as_ref()?.attempts.last()?;
    Some(Target {
        execution: execution.execution.raw(),
        key:       CheckpointKey {
            execution: execution.execution.raw(),
            firing:    attempt.firing,
            attempt:   attempt.attempt,
        },
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

/// Write the record a crash lost, from the commit found by its key.
async fn reconcile_record(
    records: &dyn PlatformRecords,
    run_id: &RunId,
    key: CheckpointKey,
    workspace: &str,
    sha: &str,
) -> Result<(), RecoveryError> {
    info!(
        run_id = %run_id,
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
    records
        .append(
            run_id,
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

/// Of the snapshots live executions name on one workspace, the one every
/// other descends from, else the last named.
async fn newest(
    workspaces: &RunWorkspaces,
    workspace: &str,
    targets: &[(Target, String)],
) -> Result<String, RecoveryError> {
    let mut chosen = &targets[0].1;
    for (_, sha) in &targets[1..] {
        if workspaces
            .is_ancestor(workspace, chosen, sha)
            .await
            .map_err(|source| RecoveryError::Workspace {
                workspace: workspace.to_string(),
                source,
            })?
        {
            chosen = sha;
        }
    }
    Ok(chosen.clone())
}

/// Verify, reset or restore the host workspace onto its target: a
/// workspace that still holds the commit is verified or reset in place; a
/// gone one, or a fresh directory with no history (a fork's first
/// acquisition), is restored from the snapshot repository.
pub async fn bring_host_to(
    workspaces: &RunWorkspaces,
    workspace: &str,
    target: &RestoreTarget,
) -> Result<WorkspaceAction, RecoveryError> {
    let failed = |source| RecoveryError::Workspace {
        workspace: workspace.to_string(),
        source,
    };
    if workspaces
        .has_commit(workspace, &target.sha)
        .await
        .map_err(failed)?
    {
        if workspaces
            .matches(workspace, &target.sha)
            .await
            .map_err(failed)?
        {
            return Ok(WorkspaceAction::Verified);
        }
        workspaces
            .reset(workspace, &target.sha)
            .await
            .map_err(failed)?;
        return Ok(WorkspaceAction::Reset);
    }
    workspaces
        .restore(workspace, target.key, &target.sha)
        .await
        .map_err(failed)?;
    Ok(WorkspaceAction::Restored)
}

/// Verify, reset or restore a sandbox workspace onto its target, through
/// the scope's environment: a retained sandbox that still holds the commit
/// is verified or reset in place; a fresh one, or one whose repository
/// lost the commit, is restored from a bundle of the snapshot.
pub async fn bring_sandbox_to(
    workspaces: &RunWorkspaces,
    env: &Arc<dyn ExecEnv>,
    workspace: &str,
    target: &RestoreTarget,
) -> Result<WorkspaceAction, RecoveryError> {
    let failed = |source| RecoveryError::Workspace {
        workspace: workspace.to_string(),
        source,
    };
    if workspaces
        .has_commit_in(env, &target.sha)
        .await
        .map_err(failed)?
    {
        if workspaces
            .matches_in(env, &target.sha)
            .await
            .map_err(failed)?
        {
            return Ok(WorkspaceAction::Verified);
        }
        workspaces
            .reset_in(env, &target.sha)
            .await
            .map_err(failed)?;
        return Ok(WorkspaceAction::Reset);
    }
    workspaces
        .restore_in(env, workspace, target.key, &target.sha)
        .await
        .map_err(failed)?;
    Ok(WorkspaceAction::Restored)
}
