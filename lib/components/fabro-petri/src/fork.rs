//! Forking a Fabro run at a checkpoint: the seam over Petri's
//! `host::fork_from` that rewind, fork and retry are built on (the
//! integration plan's F5.1).
//!
//! Fabro's checkpoint record ties a Petri position `(execution, firing)` to
//! a Git commit. A fork seeds a new run from the source's records up to such
//! a position and leaves it ready to resume, in three steps:
//!
//! 1. Petri's `fork_from` writes the new run's records into the server's store
//!    under the new run id: the same graphs, the position execution's engine
//!    log cut after the firing's routing (before its first record with
//!    `rerun_last`), the finished children the kept firings called, and a
//!    `run.started` whose `forked_from` names the source and the position. No
//!    sandbox lease is carried over, so the resume acquires the position
//!    execution's scopes fresh.
//! 2. The source's checkpoint records for every attempt the fork kept are
//!    written again under the new run, at their positions, and the snapshot
//!    repository of every workspace they name is seeded with those checkpoints'
//!    refs alone, fetched from the source's repository under the source's run
//!    scratch. That is what the resume's recovery plan
//!    ([`crate::recovery::plan`]) reads: the last durable finish of the
//!    position execution names the snapshot the fresh workspace is restored to
//!    at `scope_acquired`, on the host and in a sandbox alike.
//! 3. The fork's `run.branch` record names the run branch the restore creates
//!    (`fabro/run/<new id>`) and the commit it starts from, with the
//!    `git.identity` beside it, both at the checkpoint's position, so the hooks
//!    record nothing twice and the run's diff is measured from the fork point.
//!
//! The Fabro run row (`run.created`, the lifecycle records) and the launch in
//! resume mode are the caller's: `fabro_workflow::operations` shapes the
//! records and the server launches the worker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use fabro_checkpoint::author::GitAuthor;
use fabro_db::DbPool;
use fabro_store::platform_records::{CheckpointRecord, GitIdentityRecord, RunBranchRecord};
use fabro_store::{PlatformRecord, PlatformRecordKind, StagePosition, StoredPlatformRecord};
use fabro_types::settings::run::RunNamespace;
use fabro_types::{GitIdentity, GitIdentitySource, RunId};
use fabro_workflow::operations::{StageLabel, StageLabels};
use petri_execution::host::{self, ForkOptions, ForkOrigin, ForkPosition, HostError};
use petri_execution::inspect::{self, InspectError};
use petri_execution::{
    Access, CoordinatorEvent, ExecutionId, InvocationId, RunKey, RunStore,
    StoreError as CoordinatorStoreError,
};
use petri_runtime::ir::FiringId;
use petri_runtime::{RunOptions, Runtime};
use petri_store::StoreError;
use tokio::fs;
use tokio::process::Command;
use tracing::{debug, info};

use crate::checkpoint::{CheckpointKey, RunWorkspaces};
use crate::platform_records::{PlatformRecordError, PlatformRecords};
use crate::projection::FoldState;
use crate::projector::ProjectError;

/// One fork to seed.
pub struct ForkRequest {
    /// The run whose records are copied.
    pub source:         RunId,
    /// The new run's id: its Petri run key and its own run scratch.
    pub fork:           RunId,
    /// The source's Petri run directory (its scratch's `petri`), where its
    /// snapshot repositories are.
    pub source_run_dir: PathBuf,
    /// The fork's Petri run directory, where its snapshot repositories go.
    pub fork_run_dir:   PathBuf,
    /// The server's run store: the source is read from it, the fork is
    /// written into it.
    pub store:          Arc<dyn RunStore>,
    /// The platform records of both runs.
    pub records:        Arc<dyn PlatformRecords>,
    /// The position the source's records are kept up to.
    pub position:       ForkPosition,
    /// Whether the position's firing runs again (a retry of a failed
    /// stage) instead of keeping its finish.
    pub rerun_last:     bool,
    /// The run's settings, for its Git author and checkpoint settings.
    pub settings:       RunNamespace,
}

/// A seeded fork, not yet resumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forked {
    /// The source and position, as the fork's own run declaration records
    /// them.
    pub origin:      ForkOrigin,
    /// The checkpoints the fork kept, in the source's record order.
    pub checkpoints: Vec<KeptCheckpoint>,
    /// The snapshot the fork's workspace starts on, when the kept records
    /// name one for the position execution's last durable finish.
    pub start:       Option<KeptCheckpoint>,
}

/// A source checkpoint the fork carries over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptCheckpoint {
    pub key:       CheckpointKey,
    pub sha:       String,
    /// The Petri workspace id the commit was made in.
    pub workspace: Option<String>,
}

/// Why the fork could not be seeded.
#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    #[error("the source run's record could not be opened")]
    Open(#[source] StoreError),
    /// Petri refused the position: an unknown execution, a firing whose
    /// finish was not routed, or a position inside a child invocation (a
    /// branch of a parallel node). The message says which.
    #[error("{0}")]
    Refused(String),
    #[error("Petri could not seed the fork")]
    Seed(#[source] HostError),
    #[error("the fork's record could not be inspected")]
    Inspect(#[source] InspectError),
    #[error("the fork's coordinator log could not be read")]
    Log(#[source] CoordinatorStoreError),
    #[error("the checkpoint records could not be read or written")]
    Records(#[source] PlatformRecordError),
    #[error("the snapshot repository for `{workspace}` could not be seeded: {detail}")]
    Snapshots {
        workspace: String,
        detail:    String,
    },
}

/// Refuse a position Petri would refuse, before anything is written for
/// the fork: an execution the source does not have, or one inside a child
/// invocation (a branch of a parallel node), whose caller's firing is live
/// at every position inside it. The messages are Petri's own.
pub async fn check(
    store: &dyn RunStore,
    source: RunId,
    position: ForkPosition,
) -> Result<(), ForkError> {
    let logs = store
        .open(&RunKey::new(source.to_string()), Access::Read)
        .await
        .map_err(ForkError::Open)?;
    let state = host::stored_state(&*logs).await.map_err(ForkError::Seed)?;
    let Some(execution) = state.executions.get(&position.execution) else {
        return Err(ForkError::Refused(format!(
            "the source run has no execution {}",
            position.execution
        )));
    };
    let invocation = execution.declaration.invocation;
    if invocation != InvocationId::ROOT {
        return Err(ForkError::Refused(format!(
            "execution {} belongs to invocation {invocation}, not the root: a position inside a \
             child invocation cannot be forked",
            position.execution
        )));
    }
    Ok(())
}

/// Seed the fork: Petri's records, then the kept checkpoints, their
/// snapshots and the run branch. The new run must not exist in the store
/// yet.
pub async fn fork(request: ForkRequest) -> Result<Forked, ForkError> {
    let source_key = RunKey::new(request.source.to_string());
    let fork_key = RunKey::new(request.fork.to_string());
    let source_logs = request
        .store
        .open(&source_key, Access::Read)
        .await
        .map_err(ForkError::Open)?;

    let mut options = RunOptions::new(&request.fork_run_dir);
    options.run_key = Some(fork_key.clone());
    let runtime = Runtime::standard()
        .options(options)
        .store(Arc::clone(&request.store));
    let forked = host::fork_from(&runtime, &*source_logs, request.position, ForkOptions {
        rerun_last: request.rerun_last,
    })
    .await
    .map_err(|error| match error {
        HostError::Fork(refused) => ForkError::Refused(refused.to_string()),
        other => ForkError::Seed(other),
    })?;
    drop(source_logs);
    info!(
        source = %request.source,
        fork = %request.fork,
        position = %request.position,
        rerun_last = request.rerun_last,
        "Petri seeded the fork's records"
    );

    // What the fork kept: every attempt with a durable finish in its
    // records, and the position execution's last one.
    let fork_logs = request
        .store
        .open(&fork_key, Access::Read)
        .await
        .map_err(ForkError::Open)?;
    let inspection = inspect::inspect_run(&*fork_logs)
        .await
        .map_err(ForkError::Inspect)?;
    drop(fork_logs);
    let mut kept_keys = BTreeSet::new();
    let mut start_key = None;
    for execution in &inspection.executions {
        let Some(engine) = execution.engine.as_ref() else {
            continue;
        };
        for attempt in &engine.attempts {
            let key = CheckpointKey {
                execution: execution.execution.raw(),
                firing:    attempt.firing,
                attempt:   attempt.attempt,
            };
            kept_keys.insert(key);
            if execution.execution == request.position.execution {
                start_key = Some(key);
            }
        }
    }

    // The source's checkpoint records for the kept attempts, written again
    // under the fork at their positions.
    let source_checkpoints = request
        .records
        .read_kind(&request.source, PlatformRecordKind::Checkpoint)
        .await
        .map_err(ForkError::Records)?;
    let mut checkpoints = Vec::new();
    for stored in source_checkpoints {
        let PlatformRecord::Checkpoint(record) = &stored.record else {
            continue;
        };
        let Some(key) = checkpoint_key(record) else {
            continue;
        };
        if !kept_keys.contains(&key) {
            continue;
        }
        let Some(sha) = record.git_commit_sha.clone() else {
            continue;
        };
        let mut copied = record.clone();
        copied.attempt = Some(key.attempt);
        copied.operation = Some(key.operation());
        request
            .records
            .append(
                &request.fork,
                &PlatformRecord::Checkpoint(copied),
                Some(StagePosition {
                    execution: key.execution,
                    firing:    key.firing,
                }),
            )
            .await
            .map_err(ForkError::Records)?;
        checkpoints.push(KeptCheckpoint {
            key,
            sha,
            workspace: record.workspace.clone(),
        });
    }

    // The snapshot repositories: one per workspace the kept checkpoints
    // name, holding those checkpoints' refs alone.
    let author = request
        .settings
        .git
        .author
        .as_ref()
        .map(GitAuthor::from)
        .unwrap_or_default();
    let source_workspaces = RunWorkspaces::new(
        request.source_run_dir.clone(),
        request.source.to_string(),
        author.clone(),
        &request.settings.checkpoint,
    );
    let fork_workspaces = RunWorkspaces::new(
        request.fork_run_dir.clone(),
        request.fork.to_string(),
        author.clone(),
        &request.settings.checkpoint,
    );
    let mut by_workspace: BTreeMap<String, Vec<CheckpointKey>> = BTreeMap::new();
    for kept in &checkpoints {
        if let Some(workspace) = &kept.workspace {
            by_workspace
                .entry(workspace.clone())
                .or_default()
                .push(kept.key);
        }
    }
    for (workspace, keys) in &by_workspace {
        seed_snapshots(&source_workspaces, &fork_workspaces, workspace, keys).await?;
    }

    // The run branch the restore creates, from the checkpoint the fork
    // starts on, and the identity that authors the fork's commits.
    let start = start_key.and_then(|key| checkpoints.iter().find(|kept| kept.key == key).cloned());
    if let Some(start) = &start {
        let position = StagePosition {
            execution: start.key.execution,
            firing:    start.key.firing,
        };
        let branch = PlatformRecord::RunBranch(RunBranchRecord {
            run_branch: Some(fork_workspaces.run_branch()),
            base_sha:   Some(start.sha.clone()),
            workspace:  start.workspace.clone(),
        });
        request
            .records
            .append(&request.fork, &branch, Some(position))
            .await
            .map_err(ForkError::Records)?;
        let identity = PlatformRecord::GitIdentity(GitIdentityRecord {
            identity: GitIdentity {
                name:   author.name.clone(),
                email:  author.email.clone(),
                source: if author.is_default() {
                    GitIdentitySource::Default
                } else {
                    GitIdentitySource::Explicit
                },
            },
        });
        request
            .records
            .append(&request.fork, &identity, Some(position))
            .await
            .map_err(ForkError::Records)?;
        info!(
            fork = %request.fork,
            sha = start.sha,
            execution = start.key.execution,
            firing = start.key.firing,
            "the fork's run branch starts at the position's checkpoint"
        );
    } else {
        debug!(
            fork = %request.fork,
            "the fork keeps no checkpoint; its workspace starts empty"
        );
    }

    Ok(Forked {
        origin: forked.origin,
        checkpoints,
        start,
    })
}

/// The key a checkpoint record names: its operation identity, else its
/// position with the attempt it recorded.
fn checkpoint_key(record: &CheckpointRecord) -> Option<CheckpointKey> {
    record
        .operation
        .as_ref()
        .and_then(CheckpointKey::from_operation)
        .or_else(|| {
            Some(CheckpointKey {
                execution: record.execution,
                firing:    record.firing,
                attempt:   record.attempt?,
            })
        })
}

/// Create the fork's bare snapshot repository for `workspace` and fetch the
/// kept checkpoints' refs into it from the source's.
async fn seed_snapshots(
    source: &RunWorkspaces,
    fork: &RunWorkspaces,
    workspace: &str,
    keys: &[CheckpointKey],
) -> Result<(), ForkError> {
    let failed = |detail: String| ForkError::Snapshots {
        workspace: workspace.to_string(),
        detail,
    };
    let source_repository = source.snapshot_repository(workspace);
    if !fs::try_exists(&source_repository).await.unwrap_or(false) {
        return Err(failed(format!(
            "the source run has no snapshot repository at {}",
            source_repository.display()
        )));
    }
    let repository = fork.snapshot_repository(workspace);
    fs::create_dir_all(&repository).await.map_err(|error| {
        failed(format!(
            "{} could not be created: {error}",
            repository.display()
        ))
    })?;
    git(&repository, &["init", "-q", "--bare"])
        .await
        .map_err(failed)?;
    let mut args = vec![
        "fetch".to_string(),
        "-q".to_string(),
        source_repository.to_string_lossy().into_owned(),
    ];
    for key in keys {
        let name = key.snapshot_ref();
        args.push(format!("+{name}:{name}"));
    }
    git(&repository, &args).await.map_err(failed)?;
    debug!(
        workspace,
        refs = keys.len(),
        repository = %repository.display(),
        "the fork's snapshot repository is seeded"
    );
    Ok(())
}

/// Run `git` in `repository`; a non-zero exit is the error's detail.
async fn git<S: AsRef<str>>(repository: &std::path::Path, args: &[S]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(repository)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| format!("git could not run: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {} failed ({}): {}",
            args.first().map_or("", AsRef::as_ref),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The position a checkpoint's execution and firing name, in Petri's ids.
#[must_use]
pub fn position(execution: u64, firing: u64) -> ForkPosition {
    ForkPosition {
        execution: ExecutionId::new(execution),
        firing:    FiringId::new(firing),
    }
}

/// A fork origin as Fabro's projection shows it.
#[must_use]
pub fn origin_view(origin: &ForkOrigin) -> Option<fabro_types::ForkOrigin> {
    Some(fabro_types::ForkOrigin {
        source_run_id: origin.source.to_string().parse().ok()?,
        execution:     origin.position.execution.raw(),
        firing:        origin.position.firing.raw(),
        rerun_last:    origin.rerun_last,
    })
}

/// Where a run came from, when it is a fork: the `forked_from` of its run
/// declaration. `None` for a run that is not a fork, or that has no record
/// yet.
pub async fn origin_of(
    store: &dyn RunStore,
    run_id: RunId,
) -> Result<Option<fabro_types::ForkOrigin>, ForkError> {
    let logs = match store
        .open(&RunKey::new(run_id.to_string()), Access::Read)
        .await
    {
        Ok(logs) => logs,
        Err(StoreError::NotFound { .. }) => return Ok(None),
        Err(error) => return Err(ForkError::Open(error)),
    };
    let records = petri_execution::read_coordinator_log(&*logs)
        .await
        .map_err(ForkError::Log)?;
    Ok(records.first().and_then(|record| match &record.body {
        CoordinatorEvent::RunStarted {
            forked_from: Some(origin),
            ..
        } => origin_view(origin),
        _ => None,
    }))
}

/// The stages of a run by `(execution, firing)`, as the projector's fold
/// state names them: what a timeline labels its checkpoints with, and what
/// a fork target such as `build@2` resolves through. `views` is the pool
/// the view tables live in.
pub async fn stage_labels(views: &DbPool, run_id: RunId) -> Result<StageLabels, ProjectError> {
    let fold_json: Option<String> =
        sqlx::query_scalar("SELECT fold_json FROM petri_projection WHERE run_id = ?")
            .bind(run_id.to_string())
            .fetch_optional(views)
            .await
            .map_err(ProjectError::Database)?;
    let Some(fold_json) = fold_json else {
        return Ok(BTreeMap::new());
    };
    let state: FoldState = serde_json::from_str(&fold_json).map_err(ProjectError::Encode)?;
    Ok(state
        .stages
        .iter()
        .filter_map(|(key, stage)| {
            let (execution, firing) = key.split_once(':')?;
            Some((
                (execution.parse().ok()?, firing.parse().ok()?),
                StageLabel {
                    stage_id:  stage.shown.then(|| stage.stage_id.to_string()),
                    node_name: stage.node_name.clone(),
                    visit:     stage.visit,
                },
            ))
        })
        .collect())
}

/// The checkpoint records of a run, in seq order.
pub async fn checkpoints(
    records: &dyn PlatformRecords,
    run_id: RunId,
) -> Result<Vec<StoredPlatformRecord>, PlatformRecordError> {
    records
        .read_kind(&run_id, PlatformRecordKind::Checkpoint)
        .await
}
