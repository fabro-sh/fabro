//! Resume execution from durable records using the surviving workspace.
//! Git checkpoints are reset in place when present. Missing workspaces are
//! never reconstructed here; explicit forks fetch their source run on GitHub
//! when the worker acquires the new sandbox. Runs without Git checkpoints
//! retain execution metadata without gaining workspace backup guarantees.

use std::collections::BTreeMap;
use std::sync::Arc;

use fabro_store::{PlatformRecord, PlatformRecordKind};
use fabro_types::RunId;
use petri_execution::host::{self, HostError};
use petri_execution::inspect::{self, ExecutionInspection, InspectError};
use petri_execution::{Access, InvocationId, RunKey, RunStore};
use petri_store::StoreError;

use crate::checkpoint::{
    CHECKPOINT_FAILED_CLASS, CheckpointError, CheckpointKey, RunWorkspaces, Site,
};
use crate::platform_records::{PlatformRecordError, PlatformRecords};
use crate::workspace::{WorkspaceLookup, WorkspaceLookupError};

/// What recovery needs: the run, where its workspaces are, its records,
/// and its Git settings.
pub struct RecoveryRequest {
    pub run_id:  RunId,
    pub store:   Arc<dyn RunStore>,
    pub records: Arc<dyn PlatformRecords>,
}

impl RecoveryRequest {
    /// The request a run's settings give.
    #[must_use]
    pub fn for_run(
        run_id: RunId,
        store: Arc<dyn RunStore>,
        records: Arc<dyn PlatformRecords>,
    ) -> Self {
        Self {
            run_id,
            store,
            records,
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
        targets: BTreeMap<String, Vec<RestoreTarget>>,
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
/// on, from execution and checkpoint records. Nothing is touched.
pub async fn plan(
    store: Arc<dyn RunStore>,
    records: &dyn PlatformRecords,
    run_id: &RunId,
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
        lookup:   WorkspaceLookup::new(store, key),
        recorded: recorded_checkpoints(records, run_id).await?,
    };
    planner.targets(&inspection.executions).await
}

/// Decide how the run continues. All Git work is deferred to the worker
/// using the acquired workspace, including for the local provider.
pub async fn recover(request: RecoveryRequest) -> Result<Recovery, RecoveryError> {
    let targets = match plan(
        Arc::clone(&request.store),
        &*request.records,
        &request.run_id,
    )
    .await?
    {
        Plan::Start => return Ok(Recovery::Start),
        Plan::Failed { reason } => return Ok(Recovery::Failed { reason }),
        Plan::Resume { targets } => targets,
    };

    let recovered = targets
        .into_iter()
        .flat_map(|(workspace, targets)| {
            targets.into_iter().map(move |target| RecoveredWorkspace {
                workspace: workspace.clone(),
                sha:       target.sha,
                action:    WorkspaceAction::Deferred,
            })
        })
        .collect();
    Ok(Recovery::Resume {
        workspaces: recovered,
    })
}

/// The decision over one run's durable records.
struct Planner {
    lookup:   WorkspaceLookup,
    recorded: BTreeMap<CheckpointKey, (Option<String>, String)>,
}

impl Planner {
    /// The snapshot each live execution's workspace must sit on. A live
    /// execution is one whose log records no exit: `inspect_run` reports
    /// it as incomplete. A workspace several live executions share is
    /// brought to the newest of their snapshots.
    async fn targets(&self, executions: &[ExecutionInspection]) -> Result<Plan, RecoveryError> {
        let mut candidates: BTreeMap<String, Vec<RestoreTarget>> = BTreeMap::new();
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
            for workspace in owned {
                if let Some((recorded_workspace, sha)) = self.recorded.get(&key) {
                    if recorded_workspace
                        .as_deref()
                        .is_none_or(|id| id == workspace)
                    {
                        candidates
                            .entry(workspace)
                            .or_default()
                            .push(RestoreTarget {
                                key,
                                sha: sha.clone(),
                            });
                    }
                }
            }
        }

        Ok(Plan::Resume {
            targets: candidates,
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

/// Verify or reset an existing workspace. A missing commit is an error;
/// this operation never reconstructs a lost workspace.
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
    Err(failed(CheckpointError::MissingCommit {
        sha: target.sha.clone(),
    }))
}
