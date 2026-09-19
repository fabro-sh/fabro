//! A run's checkpoint timeline, and the runs made from it: fork, rewind and
//! retry (the integration plan's F5.1).
//!
//! The timeline is the run's `checkpoint` platform records, labelled with
//! the stages the projector folded them onto. A fork resolves a target on
//! that timeline (`@ordinal`, a node, or `node@visit`; the latest checkpoint
//! by default), creates the new run's row (`fabro_workflow::operations`),
//! seeds its records, checkpoints, snapshots and run branch from the source
//! (`fabro_petri::fork`), and queues it in resume mode, so its worker
//! acquires a fresh workspace, restores the checkpoint's commit into it and
//! continues from the position. A rewind is a fork of a terminal run that
//! archives the source and records `run.superseded` on it; a retry is a
//! fork of a terminal run at its last checkpoint, with the failed stage run
//! again when the run failed on one.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fabro_api::types as api;
use fabro_config::Storage;
use fabro_petri::SqliteRunStore;
use fabro_petri::fork::{self as petri_fork, ForkError, ForkRequest};
use fabro_petri::petri::RunStore;
use fabro_petri::platform_records::SqlitePlatformRecords;
use fabro_store::{PlatformRecordKind, RunProjection};
use fabro_types::{FailureReason, Principal, RunId};
use fabro_util::error as error_util;
use fabro_workflow::Error as WorkflowError;
use fabro_workflow::operations::{self, ForkTarget, ResolvedForkTarget, RunTimeline};
use tracing::{error, warn};

use super::super::{
    ApiError, AppState, RequireRunManagementTarget, RequiredUser, parse_run_id_path, run_records,
};
use super::lifecycle::{ArchiveAction, queue_run, run_archive_operation, run_response};
use super::runs::run_provenance;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/timeline", get(run_timeline))
        .route("/runs/{id}/fork", post(fork_run))
        .route("/runs/{id}/rewind", post(rewind_run))
        .route("/runs/{id}/retry", post(retry_run))
}

/// Which operation a fork is made for: what it requires of the source and
/// what it records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForkKind {
    Fork,
    Rewind,
    Retry,
}

/// A fork made and queued.
struct ForkOutcome {
    source_run_id: RunId,
    new_run_id:    RunId,
    target:        ResolvedForkTarget,
    rerun_last:    bool,
}

async fn run_timeline(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let projection = match run_records::require_projection(state.as_ref(), id).await {
        Ok(projection) => projection,
        Err(err) => return err.into_response(),
    };
    match timeline(state.as_ref(), id).await {
        Ok(timeline) => Json(timeline_response(&timeline, &projection)).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn fork_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<api::ForkRequest>>,
) -> Response {
    let target = match parse_fork_target(body.and_then(|Json(body)| body.target)) {
        Ok(target) => target,
        Err(err) => return err.into_response(),
    };
    match fork_at(state.as_ref(), id, actor, &headers, ForkKind::Fork, target).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(api::ForkResponse {
                source_run_id:  outcome.source_run_id.to_string(),
                new_run_id:     outcome.new_run_id.to_string(),
                target:         outcome.target.response_target(),
                checkpoint_sha: outcome.target.checkpoint_sha.clone(),
                execution:      outcome.target.position.execution,
                firing:         outcome.target.position.firing,
                rerun_last:     outcome.rerun_last,
            }),
        )
            .into_response(),
        Err(err) => err.into_response(),
    }
}

async fn rewind_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<api::RewindRequest>>,
) -> Response {
    let target = match parse_fork_target(body.and_then(|Json(body)| body.target)) {
        Ok(target) => target,
        Err(err) => return err.into_response(),
    };
    let outcome = match fork_at(
        state.as_ref(),
        id,
        actor.clone(),
        &headers,
        ForkKind::Rewind,
        target,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(err) => return err.into_response(),
    };
    // The source is replaced: archived, and marked with what replaced it.
    // A failed archive leaves the new run in place and says so.
    let archived = run_archive_operation(state.as_ref(), &id, Some(actor), ArchiveAction::Archive)
        .await
        .map(|_| ());
    if archived.is_ok() {
        let record = operations::superseded_record(outcome.new_run_id, &outcome.target);
        if let Err(err) = run_records::append(state.as_ref(), id, record).await {
            error!(
                source_run_id = %id,
                new_run_id = %outcome.new_run_id,
                error = %err,
                "the rewound run was archived but its superseded record was not written"
            );
        }
    }
    let (status, archive_error) = match archived {
        Ok(()) => (StatusCode::OK, None),
        Err(err) => (StatusCode::MULTI_STATUS, Some(err.to_string())),
    };
    (
        status,
        Json(api::RewindResponse {
            source_run_id: outcome.source_run_id.to_string(),
            new_run_id: outcome.new_run_id.to_string(),
            target: outcome.target.response_target(),
            checkpoint_sha: outcome.target.checkpoint_sha.clone(),
            execution: outcome.target.position.execution,
            firing: outcome.target.position.firing,
            archived: archive_error.is_none(),
            archive_error,
        }),
    )
        .into_response()
}

async fn retry_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    match fork_at(state.as_ref(), id, actor, &headers, ForkKind::Retry, None).await {
        Ok(outcome) => run_response(state.as_ref(), outcome.new_run_id, StatusCode::CREATED).await,
        Err(err) => err.into_response(),
    }
}

/// The run's timeline: its checkpoint records, labelled through the
/// projector's fold state.
async fn timeline(state: &AppState, id: RunId) -> Result<RunTimeline, ApiError> {
    let checkpoints = state
        .stores
        .run_summaries
        .platform_records()
        .read_kind(&id, PlatformRecordKind::Checkpoint)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let labels = petri_fork::stage_labels(&state.stores.run_summaries.pool(), id)
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(RunTimeline::build(&checkpoints, &labels))
}

fn timeline_response(
    timeline: &RunTimeline,
    projection: &RunProjection,
) -> api::RunTimelineResponse {
    api::RunTimelineResponse {
        entries:     timeline
            .entries
            .iter()
            .map(|entry| api::TimelineEntryResponse {
                ordinal:        u64::try_from(entry.ordinal).unwrap_or(u64::MAX),
                checkpoint_seq: entry.checkpoint_seq,
                execution:      entry.position.execution,
                firing:         entry.position.firing,
                attempt:        entry.position.attempt,
                stage:          entry.stage_id.clone(),
                node_name:      entry.node_name.clone(),
                visit:          entry.visit,
                workspace:      entry.workspace.clone(),
                run_commit_sha: entry.run_commit_sha.clone(),
                diff_summary:   entry.diff_summary.as_ref().map(|summary| api::DiffSummary {
                    files_changed: summary.files_changed,
                    additions:     summary.additions,
                    deletions:     summary.deletions,
                }),
            })
            .collect(),
        forked_from: projection.forked_from.as_ref().map(fork_origin_response),
    }
}

fn fork_origin_response(origin: &fabro_types::ForkOrigin) -> api::ForkOrigin {
    api::ForkOrigin {
        source_run_id: origin.source_run_id.to_string(),
        execution:     origin.execution,
        firing:        origin.firing,
        rerun_last:    origin.rerun_last,
    }
}

fn parse_fork_target(target: Option<String>) -> Result<Option<ForkTarget>, ApiError> {
    target
        .map(|target| {
            target
                .parse::<ForkTarget>()
                .map_err(workflow_operation_error)
        })
        .transpose()
}

/// Make the fork: check the source, resolve the target, create the new
/// run's row, seed it from the source, and queue it in resume mode.
async fn fork_at(
    state: &AppState,
    id: RunId,
    actor: Principal,
    headers: &HeaderMap,
    kind: ForkKind,
    target: Option<ForkTarget>,
) -> Result<ForkOutcome, ApiError> {
    let source = run_records::require_projection(state, id).await?;
    match kind {
        ForkKind::Fork => operations::ensure_forkable(&source, &id),
        ForkKind::Rewind => operations::ensure_rewindable(&source, &id),
        ForkKind::Retry => operations::ensure_retryable(&source, &id),
    }
    .map_err(workflow_operation_error)?;
    let timeline = timeline(state, id).await?;
    let entry = match kind {
        ForkKind::Retry => timeline.latest(),
        ForkKind::Fork | ForkKind::Rewind => timeline.resolve_or_latest(target.as_ref()),
    }
    .map_err(workflow_operation_error)?;
    let resolved = ResolvedForkTarget::of(entry).map_err(workflow_operation_error)?;
    let rerun_last = kind == ForkKind::Retry && operations::reruns_last(source.status);

    // A position Petri would refuse is refused before the new run exists.
    let position = petri_fork::position(resolved.position.execution, resolved.position.firing);
    let store: Arc<dyn RunStore> = state
        .petri_projector
        .observe_store(Arc::new(SqliteRunStore::new(state.db_pool.clone())));
    petri_fork::check(store.as_ref(), id, position)
        .await
        .map_err(|err| fork_error(&err))?;

    let new_run_id = RunId::new();
    let storage = Storage::new(state.server_storage_dir());
    let source_run_dir = storage.run_scratch(&id).root().to_path_buf();
    let run_dir = storage.run_scratch(&new_run_id).root().to_path_buf();
    let provenance = (kind == ForkKind::Retry).then(|| run_provenance(headers, &actor));
    operations::persist_forked_run(state.store_ref().as_ref(), &operations::ForkedRunInput {
        source: &source,
        new_run_id,
        run_dir: run_dir.clone(),
        checkpoint_sha: resolved.checkpoint_sha.clone(),
        provenance,
        web_url: state.run_web_url(&new_run_id),
        retried_from: (kind == ForkKind::Retry).then_some(id),
    })
    .await
    .map_err(workflow_operation_error)?;

    let seeded = petri_fork::fork(ForkRequest {
        source: id,
        fork: new_run_id,
        source_run_dir: source_run_dir.join("petri"),
        fork_run_dir: run_dir.join("petri"),
        store,
        records: Arc::new(SqlitePlatformRecords::new(Arc::clone(
            &state.stores.run_summaries,
        ))),
        position,
        rerun_last,
        settings: source.spec.settings.run.clone(),
    })
    .await;
    if let Err(err) = seeded {
        // The new run's row exists and holds nothing to continue from: it
        // is reported failed with the reason, rather than left submitted.
        let message = error_util::collect_chain(&err).join(": ");
        warn!(source_run_id = %id, new_run_id = %new_run_id, error = %message, "the fork could not be seeded");
        if let Err(record_err) = run_records::lifecycle(
            state,
            new_run_id,
            run_records::failed(FailureReason::WorkflowError, message.clone()),
        )
        .await
        {
            error!(new_run_id = %new_run_id, error = %record_err, "the fork's failure was not recorded");
        }
        return Err(fork_error(&err));
    }

    // The fork continues as a run left in flight does: queued in resume
    // mode, on the projection its seeded records folded to.
    let projection = run_records::require_projection(state, new_run_id).await?;
    queue_run(state, new_run_id, &projection, true, actor).await?;
    Ok(ForkOutcome {
        source_run_id: id,
        new_run_id,
        target: resolved,
        rerun_last,
    })
}

/// A refused position is the caller's mistake; anything else is the
/// server's.
fn fork_error(err: &ForkError) -> ApiError {
    let message = error_util::collect_chain(err).join(": ");
    let status = match err {
        ForkError::Refused(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError::new(status, message)
}

fn workflow_operation_error(err: WorkflowError) -> ApiError {
    match err {
        WorkflowError::Parse(message) | WorkflowError::Validation(message) => {
            ApiError::bad_request(message)
        }
        WorkflowError::Precondition(message) => ApiError::new(StatusCode::CONFLICT, message),
        WorkflowError::RunNotFound(_) => ApiError::not_found("Run not found."),
        err => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}
