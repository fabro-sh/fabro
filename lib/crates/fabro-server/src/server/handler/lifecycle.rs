use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;

use super::super::{
    ApiError, AppState, AskFabroReadiness, BatchDeleteRunsRequest, BatchDeleteRunsResponse,
    BatchDeleteRunsResult, BatchDeleteRunsResultOutcome, BatchDeleteRunsSummary,
    BatchRunLifecycleRequest, BatchRunLifecycleResponse, BatchRunLifecycleResult,
    BatchRunLifecycleResultOutcome, BatchRunLifecycleSummary, DeleteRunOutcome, DeleteRunSandbox,
    DenyRunRequest, FailureReason, ForkRequest, ForkResponse, HeaderMap, IntoResponse, Json, Path,
    PendingReason, Principal, RequireRunManagementTarget, RequiredUser, Response, RewindRequest,
    RewindResponse, Router, RunAnswerTransport, RunControlAction, RunExecutionMode, RunId,
    RunRunnableSource, RunStatus, StartRunRequest, State, StatusCode, Storage,
    TimelineEntryResponse, WORKER_CANCEL_GRACE, WorkflowError, append_control_request,
    clear_live_run_state, delete_run_internal, durable_run_status, get, load_pending_control,
    managed_run, operations, parse_run_id_path, persist_cancelled_run_status, post,
    reject_if_archived, sleep, update_live_run_from_event, workflow_event,
};
use super::runs::run_provenance;
use crate::worker_runtime::WorkerRef;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/start", post(start_run))
        .route("/runs/{id}/approve", post(approve_run))
        .route("/runs/{id}/deny", post(deny_run))
        .route("/runs/{id}/pause", post(pause_run))
        .route("/runs/{id}/unpause", post(unpause_run))
        .route("/runs/archive", post(batch_archive_runs))
        .route("/runs/delete", post(batch_delete_runs))
        .route("/runs/unarchive", post(batch_unarchive_runs))
        .route("/runs/{id}/archive", post(archive_run))
        .route("/runs/{id}/rewind", post(rewind_run))
        .route("/runs/{id}/retry", post(retry_run))
        .route("/runs/{id}/fork", post(fork_run))
        .route("/runs/{id}/timeline", get(run_timeline))
        .route("/runs/{id}/unarchive", post(unarchive_run))
}

async fn run_response(state: &AppState, id: RunId, status: StatusCode) -> Response {
    match state.store.get_cached_summary(&id, Utc::now()).await {
        Ok(Some(summary)) => {
            (status, Json(state.decorate_run_summary(summary).await)).into_response()
        }
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn start_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    body: Option<Json<StartRunRequest>>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let resume = body.is_some_and(|Json(req)| req.resume);

    match queue_run_start(state.as_ref(), id, resume, actor).await {
        Ok(()) => run_response(state.as_ref(), id, StatusCode::OK).await,
        Err(err) => err.into_response(),
    }
}

pub(in crate::server) async fn queue_run_start(
    state: &AppState,
    id: RunId,
    resume: bool,
    actor: Principal,
) -> Result<(), ApiError> {
    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get(&id) {
            if matches!(
                managed_run.status,
                RunStatus::Pending { .. }
                    | RunStatus::Runnable
                    | RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. }
            ) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    if resume {
                        "an engine process is still running for this run — cannot resume"
                    } else if matches!(
                        managed_run.status,
                        RunStatus::Pending { .. } | RunStatus::Runnable
                    ) {
                        "start has already been requested for this run"
                    } else {
                        "an engine process is still running for this run — cannot start"
                    },
                ));
            }
        }
    }

    let Ok(run_store) = state.store.open_run(&id).await else {
        return Err(ApiError::not_found("Run not found."));
    };
    let run_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load run state: {err}"),
            ));
        }
    };

    if resume {
        if run_state.current_checkpoint().is_none() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "no checkpoint to resume from",
            ));
        }
    } else {
        let status = run_state.status;
        if !matches!(status, RunStatus::Submitted) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("cannot start run: status is {status}, expected submitted"),
            ));
        }
    }

    let run_dir = Storage::new(state.server_storage_dir())
        .run_scratch(&id)
        .root()
        .to_path_buf();
    let dot_source = run_state.spec.graph_source.clone().unwrap_or_default();
    let approval_required = !resume
        && matches!(
            &actor,
            Principal::Worker { run_id } if run_state.parent_id == Some(*run_id)
        );
    if let Err(err) =
        workflow_event::append_event(&run_store, &id, &workflow_event::Event::RunStartRequested {
            resume,
            actor: Some(actor.clone()),
        })
        .await
    {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        ));
    }
    let (next_status, next_event) = if approval_required {
        (
            RunStatus::Pending {
                reason: PendingReason::ApprovalRequired,
            },
            workflow_event::Event::RunPending {
                reason: PendingReason::ApprovalRequired,
                actor:  Some(actor),
            },
        )
    } else {
        (RunStatus::Runnable, workflow_event::Event::RunRunnable {
            source: RunRunnableSource::StartRequested,
            actor:  Some(actor),
        })
    };
    if let Err(err) = workflow_event::append_event(&run_store, &id, &next_event).await {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        ));
    }

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            id,
            managed_run(
                dot_source,
                next_status,
                id.created_at(),
                run_dir,
                if resume {
                    RunExecutionMode::Resume
                } else {
                    RunExecutionMode::Start
                },
            ),
        );
    }

    if !approval_required {
        state.scheduler_notify.notify_one();
    }
    Ok(())
}

async fn approve_run(
    RequiredUser(user): RequiredUser,
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let Ok(run_store) = state.store.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load run state: {err}"),
            )
            .into_response();
        }
    };
    if !matches!(run_state.status, RunStatus::Pending {
        reason: PendingReason::ApprovalRequired,
    }) {
        return ApiError::new(StatusCode::CONFLICT, "Run is not pending approval.").into_response();
    }

    let actor = Some(Principal::User(user));
    for event in [
        workflow_event::Event::RunApproved {
            actor: actor.clone(),
        },
        workflow_event::Event::RunRunnable {
            source: RunRunnableSource::Approved,
            actor,
        },
    ] {
        if let Err(err) = workflow_event::append_event(&run_store, &id, &event).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&id) {
            managed_run.status = RunStatus::Runnable;
        } else {
            let run_dir = Storage::new(state.server_storage_dir())
                .run_scratch(&id)
                .root()
                .to_path_buf();
            let dot_source = run_state.spec.graph_source.clone().unwrap_or_default();
            runs.insert(
                id,
                managed_run(
                    dot_source,
                    RunStatus::Runnable,
                    id.created_at(),
                    run_dir,
                    RunExecutionMode::Start,
                ),
            );
        }
    }

    state.scheduler_notify.notify_one();
    run_response(state.as_ref(), id, StatusCode::OK).await
}

async fn deny_run(
    RequiredUser(user): RequiredUser,
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    body: Option<Json<DenyRunRequest>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let reason = body
        .and_then(|Json(req)| req.reason)
        .map(|reason| reason.trim().to_string())
        .filter(|reason| !reason.is_empty());
    let message = reason
        .clone()
        .unwrap_or_else(|| "Not approved for execution".to_string());
    let Ok(run_store) = state.store.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load run state: {err}"),
            )
            .into_response();
        }
    };
    if !matches!(run_state.status, RunStatus::Pending {
        reason: PendingReason::ApprovalRequired,
    }) {
        return ApiError::new(StatusCode::CONFLICT, "Run is not pending approval.").into_response();
    }

    let actor = Some(Principal::User(user));
    let denied_event = workflow_event::Event::RunDenied {
        reason: reason.clone(),
        actor,
    };
    if let Err(err) = workflow_event::append_event(&run_store, &id, &denied_event).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    let failure_event = workflow_event::Event::workflow_run_failed_from_error(
        &WorkflowError::engine(message.clone()),
        fabro_types::RunTiming::default(),
        FailureReason::ApprovalDenied,
        None,
        None,
        None,
        None,
    );
    if let Err(err) = workflow_event::append_event(&run_store, &id, &failure_event).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get_mut(&id) {
            managed_run.status = RunStatus::Failed {
                reason: FailureReason::ApprovalDenied,
            };
            managed_run.error = Some(message);
            clear_live_run_state(managed_run);
        }
    }

    run_response(state.as_ref(), id, StatusCode::OK).await
}

fn schedule_worker_force_stop(state: Arc<AppState>, run_id: RunId, worker_ref: WorkerRef) {
    tokio::spawn(async move {
        sleep(WORKER_CANCEL_GRACE).await;
        let current_ref = {
            let runs = state.runs.lock().expect("runs lock poisoned");
            runs.get(&run_id).and_then(|run| run.worker_ref.clone())
        };
        if current_ref.as_ref() == Some(&worker_ref)
            && state.worker_runtime.is_alive(&worker_ref).await
        {
            state.worker_runtime.force_stop(&worker_ref).await;
        }
    });
}

async fn cancel_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let cancel_target = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get_mut(&id) {
            Some(managed_run) => match managed_run.status {
                RunStatus::Submitted
                | RunStatus::Pending { .. }
                | RunStatus::Runnable
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Blocked { .. }
                | RunStatus::Paused { .. } => {
                    let persist_cancelled_status = matches!(
                        managed_run.status,
                        RunStatus::Submitted | RunStatus::Pending { .. } | RunStatus::Runnable
                    );
                    if persist_cancelled_status {
                        managed_run.status = RunStatus::Failed {
                            reason: FailureReason::Cancelled,
                        };
                    }
                    Some((
                        persist_cancelled_status,
                        managed_run.answer_transport.clone(),
                        managed_run.cancel_token.clone(),
                        managed_run.cancel_tx.take(),
                        managed_run.worker_ref.clone(),
                    ))
                }
                _ => {
                    return ApiError::new(StatusCode::CONFLICT, "Run is not cancellable.")
                        .into_response();
                }
            },
            None => None,
        }
    };
    let Some((persist_cancelled_status, answer_transport, cancel_token, cancel_tx, worker_ref)) =
        cancel_target
    else {
        return unmanaged_cancel_response(state.as_ref(), id, actor, pending_control).await;
    };

    if pending_control != Some(RunControlAction::Cancel) {
        if let Err(err) =
            append_control_request(state.as_ref(), id, RunControlAction::Cancel, Some(actor)).await
        {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    if let Some(token) = &cancel_token {
        token.cancel();
    }
    let sent_in_process_cancel = if let Some(cancel_tx) = cancel_tx {
        let _ = cancel_tx.send(());
        true
    } else {
        false
    };
    let delivered_control = if let Some(answer_transport) = answer_transport {
        if sent_in_process_cancel
            && matches!(answer_transport, RunAnswerTransport::InProcess { .. })
        {
            true
        } else {
            answer_transport.cancel_run().await.is_ok()
        }
    } else {
        false
    };
    if !delivered_control {
        if let Some(worker_ref) = worker_ref {
            state.worker_runtime.request_stop(&worker_ref).await;
            schedule_worker_force_stop(Arc::clone(&state), id, worker_ref);
        }
    }

    if persist_cancelled_status {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), id).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    run_response(state.as_ref(), id, StatusCode::OK).await
}

async fn unmanaged_cancel_response(
    state: &AppState,
    id: RunId,
    actor: Principal,
    pending_control: Option<RunControlAction>,
) -> Response {
    match durable_run_status(state, id).await {
        Ok(Some(status)) if status.is_terminal() => ApiError::new(
            StatusCode::CONFLICT,
            "Run is already terminal and cannot be cancelled.",
        )
        .into_response(),
        Ok(Some(RunStatus::Submitted | RunStatus::Pending { .. } | RunStatus::Runnable)) => {
            if pending_control != Some(RunControlAction::Cancel) {
                if let Err(err) =
                    append_control_request(state, id, RunControlAction::Cancel, Some(actor)).await
                {
                    return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                        .into_response();
                }
            }
            match persist_cancelled_run_status(state, id).await {
                Ok(()) => run_response(state, id, StatusCode::OK).await,
                Err(err) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response(),
            }
        }
        Ok(Some(_)) => {
            ApiError::new(StatusCode::CONFLICT, "Run is not cancellable.").into_response()
        }
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

/// How `pause_run` should enact the transition, chosen from the current run
/// status.
enum PauseMode {
    /// Worker is running; ask it to pause via the worker control bus. Status
    /// flips to `Paused` once the worker acknowledges.
    Transport { transport: RunAnswerTransport },
    /// Worker is blocked on a human gate; flip to `Paused` directly by
    /// appending `RunPaused` ourselves.
    AppendEvent,
}

/// How `unpause_run` should enact the transition.
enum UnpauseMode {
    /// No outstanding block; ask the worker to resume via the worker control
    /// bus.
    Transport { transport: RunAnswerTransport },
    /// Was paused while blocked; append `RunUnpaused` and let the reducer
    /// restore the underlying blocked state from `Paused { prior_block }`.
    AppendEvent,
}

async fn pause_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let mode = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) if managed_run.status == RunStatus::Running => {
                let Some(transport) = managed_run.answer_transport.clone() else {
                    return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.")
                        .into_response();
                };
                PauseMode::Transport { transport }
            }
            Some(managed_run) if matches!(managed_run.status, RunStatus::Blocked { .. }) => {
                PauseMode::AppendEvent
            }
            Some(_) => {
                return ApiError::new(StatusCode::CONFLICT, "Run is not pausable.").into_response();
            }
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control.is_some() {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Run control request is already pending.",
        )
        .into_response();
    }
    if let Err(err) = append_control_request(
        state.as_ref(),
        id,
        RunControlAction::Pause,
        Some(Principal::User(subject.0.clone())),
    )
    .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    match mode {
        PauseMode::Transport { transport } => {
            if transport.pause_run().await.is_err() {
                return ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Failed to deliver pause request to the active run.",
                )
                .into_response();
            }
        }
        PauseMode::AppendEvent => {
            if let Some(response) = synchronous_transition(state.as_ref(), id, |events| {
                events.push(workflow_event::Event::RunPaused);
            })
            .await
            {
                return response;
            }
        }
    }

    run_response(state.as_ref(), id, StatusCode::OK).await
}

async fn unpause_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let pending_control = match load_pending_control(state.as_ref(), id).await {
        Ok(pending_control) => pending_control,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let mode = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => match managed_run.status {
                RunStatus::Paused {
                    prior_block: Some(_),
                } => UnpauseMode::AppendEvent,
                RunStatus::Paused { prior_block: None } => {
                    let Some(transport) = managed_run.answer_transport.clone() else {
                        return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.")
                            .into_response();
                    };
                    UnpauseMode::Transport { transport }
                }
                _ => {
                    return ApiError::new(StatusCode::CONFLICT, "Run is not paused.")
                        .into_response();
                }
            },
            None => return ApiError::not_found("Run not found.").into_response(),
        }
    };

    if pending_control.is_some() {
        return ApiError::new(
            StatusCode::CONFLICT,
            "Run control request is already pending.",
        )
        .into_response();
    }
    if let Err(err) = append_control_request(
        state.as_ref(),
        id,
        RunControlAction::Unpause,
        Some(Principal::User(subject.0.clone())),
    )
    .await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    match mode {
        UnpauseMode::Transport { transport } => {
            if transport.unpause_run().await.is_err() {
                return ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Failed to deliver unpause request to the active run.",
                )
                .into_response();
            }
        }
        UnpauseMode::AppendEvent => {
            if let Some(response) = synchronous_transition(state.as_ref(), id, |events| {
                events.push(workflow_event::Event::RunUnpaused);
            })
            .await
            {
                return response;
            }
        }
    }

    run_response(state.as_ref(), id, StatusCode::OK).await
}

async fn archive_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    run_archive_action(state, actor, id, ArchiveAction::Archive).await
}

async fn unarchive_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
) -> Response {
    run_archive_action(state, actor, id, ArchiveAction::Unarchive).await
}

async fn batch_archive_runs(
    RequiredUser(user): RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(request): Json<BatchRunLifecycleRequest>,
) -> Response {
    batch_run_archive_action(
        state,
        Principal::User(user),
        request,
        ArchiveAction::Archive,
    )
    .await
}

async fn batch_unarchive_runs(
    RequiredUser(user): RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(request): Json<BatchRunLifecycleRequest>,
) -> Response {
    batch_run_archive_action(
        state,
        Principal::User(user),
        request,
        ArchiveAction::Unarchive,
    )
    .await
}

async fn batch_delete_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(request): Json<BatchDeleteRunsRequest>,
) -> Response {
    let force = request.force;
    let ids = match validate_batch_run_ids(request.run_ids) {
        Ok(ids) => ids,
        Err(err) => return err.into_response(),
    };

    let mut results = Vec::with_capacity(ids.len());
    for id in ids {
        results.push(batch_delete_run_item(state.as_ref(), id, force).await);
    }

    let requested = results.len() as u64;
    let succeeded = results.iter().filter(|result| result.ok).count() as u64;
    (
        StatusCode::OK,
        Json(BatchDeleteRunsResponse {
            results,
            summary: BatchDeleteRunsSummary {
                requested,
                succeeded,
                failed: requested - succeeded,
            },
        }),
    )
        .into_response()
}

async fn rewind_run(
    subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<RewindRequest>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let target = match parse_fork_target(request.target) {
        Ok(target) => target,
        Err(err) => return err.into_response(),
    };
    let input = operations::RewindInput { run_id: id, target };
    match Box::pin(operations::rewind(
        &state.store,
        &input,
        Some(Principal::User(subject.0.clone())),
    ))
    .await
    {
        Ok(operations::RewindOutcome::Full {
            source_run_id,
            new_run_id,
            target,
        }) => (
            StatusCode::OK,
            Json(RewindResponse {
                source_run_id: source_run_id.to_string(),
                new_run_id:    new_run_id.to_string(),
                target:        target.response_target(),
                archived:      true,
                archive_error: None,
            }),
        )
            .into_response(),
        Ok(operations::RewindOutcome::Partial {
            source_run_id,
            new_run_id,
            target,
            archive_error,
        }) => (
            StatusCode::MULTI_STATUS,
            Json(RewindResponse {
                source_run_id: source_run_id.to_string(),
                new_run_id:    new_run_id.to_string(),
                target:        target.response_target(),
                archived:      false,
                archive_error: Some(archive_error),
            }),
        )
            .into_response(),
        Err(err) => workflow_operation_error_response(err),
    }
}

async fn fork_run(
    _subject: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<ForkRequest>>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let target = match parse_fork_target(request.target) {
        Ok(target) => target,
        Err(err) => return err.into_response(),
    };
    let input = operations::ForkRunInput {
        source_run_id: id,
        target,
    };
    match Box::pin(operations::fork_run(&state.store, &input)).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(ForkResponse {
                source_run_id: outcome.source_run_id.to_string(),
                new_run_id:    outcome.new_run_id.to_string(),
                target:        outcome.target.response_target(),
            }),
        )
            .into_response(),
        Err(err) => workflow_operation_error_response(err),
    }
}

async fn retry_run(
    RequiredUser(user): RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let actor = Principal::User(user);
    let new_run_id = RunId::new();
    let input = operations::RetryRunInput {
        source_run_id: id,
        new_run_id,
        provenance: Some(run_provenance(&headers, &actor)),
        web_url: state.run_web_url(&new_run_id),
    };
    match Box::pin(operations::retry_run(&state.store, &input)).await {
        Ok(outcome) => {
            let new_run_id = outcome.new_run_id;
            if let Err(err) = queue_run_start(state.as_ref(), new_run_id, false, actor).await {
                return err.into_response();
            }
            run_response(state.as_ref(), new_run_id, StatusCode::CREATED).await
        }
        Err(err) => workflow_operation_error_response(err),
    }
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
    match operations::timeline(&state.store, &id).await {
        Ok(entries) => Json(
            entries
                .into_iter()
                .map(|entry| TimelineEntryResponse {
                    ordinal:        std::num::NonZeroU64::new(entry.ordinal as u64)
                        .expect("timeline ordinals start at 1"),
                    node_name:      entry.node_name,
                    visit:          std::num::NonZeroU64::new(entry.visit as u64)
                        .expect("timeline visits start at 1"),
                    checkpoint_seq: std::num::NonZeroU64::new(u64::from(entry.checkpoint_seq))
                        .expect("checkpoint event sequence starts at 1"),
                    run_commit_sha: entry.run_commit_sha,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(err) => workflow_operation_error_response(err),
    }
}

fn parse_fork_target(target: Option<String>) -> Result<Option<operations::ForkTarget>, ApiError> {
    target
        .map(|target| {
            target
                .parse::<operations::ForkTarget>()
                .map_err(|err| ApiError::bad_request(err.to_string()))
        })
        .transpose()
}

fn workflow_operation_error_response(err: WorkflowError) -> Response {
    match err {
        WorkflowError::Parse(message) | WorkflowError::Validation(message) => {
            ApiError::bad_request(message).into_response()
        }
        WorkflowError::ValidationFailed { .. } => {
            ApiError::bad_request("Validation failed").into_response()
        }
        WorkflowError::Precondition(message) => {
            ApiError::new(StatusCode::CONFLICT, message).into_response()
        }
        WorkflowError::RunNotFound(_) => ApiError::not_found("Run not found.").into_response(),
        WorkflowError::Unsupported(message) => {
            ApiError::new(StatusCode::NOT_IMPLEMENTED, message).into_response()
        }
        err => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Clone, Copy)]
enum ArchiveAction {
    Archive,
    Unarchive,
}

const MAX_BATCH_RUN_IDS: usize = 250;

async fn batch_run_archive_action(
    state: Arc<AppState>,
    actor: Principal,
    request: BatchRunLifecycleRequest,
    action: ArchiveAction,
) -> Response {
    let ids = match validate_batch_run_ids(request.run_ids) {
        Ok(ids) => ids,
        Err(err) => return err.into_response(),
    };

    // Resolve Ask Fabro readiness once per batch instead of inside each
    // per-item summary lookup; readiness is identical for every run in the
    // request and resolving it performs LLM credential work.
    let readiness = state.ask_fabro_readiness().await;
    let mut results = Vec::with_capacity(ids.len());
    for id in ids {
        results.push(
            batch_run_archive_item(state.as_ref(), &readiness, actor.clone(), id, action).await,
        );
    }

    let requested = results.len() as u64;
    let succeeded = results.iter().filter(|result| result.ok).count() as u64;
    (
        StatusCode::OK,
        Json(BatchRunLifecycleResponse {
            results,
            summary: BatchRunLifecycleSummary {
                requested,
                succeeded,
                failed: requested - succeeded,
            },
        }),
    )
        .into_response()
}

fn validate_batch_run_ids(run_ids: Vec<String>) -> Result<Vec<RunId>, ApiError> {
    if run_ids.is_empty() {
        return Err(ApiError::bad_request(
            "run_ids must contain at least one run ID.",
        ));
    }
    if run_ids.len() > MAX_BATCH_RUN_IDS {
        return Err(ApiError::bad_request(format!(
            "run_ids must contain no more than {MAX_BATCH_RUN_IDS} run IDs.",
        )));
    }

    let mut seen = HashSet::with_capacity(run_ids.len());
    let mut ids = Vec::with_capacity(run_ids.len());
    for raw in run_ids {
        let id = raw.parse::<RunId>().map_err(|_| {
            ApiError::bad_request(format!("run_ids contains invalid run ID: {raw}"))
        })?;
        if !seen.insert(id) {
            return Err(ApiError::bad_request(
                "run_ids must not contain duplicate IDs.",
            ));
        }
        ids.push(id);
    }
    Ok(ids)
}

async fn batch_delete_run_item(state: &AppState, id: RunId, force: bool) -> BatchDeleteRunsResult {
    match delete_run_internal(state, id, force).await {
        Ok(DeleteRunOutcome::Deleted) => {
            batch_delete_success(id, BatchDeleteRunsResultOutcome::Deleted, None)
        }
        Ok(DeleteRunOutcome::AlreadyAbsent) => {
            batch_delete_success(id, BatchDeleteRunsResultOutcome::AlreadyAbsent, None)
        }
        Ok(DeleteRunOutcome::Preserved(response)) => batch_delete_success(
            id,
            BatchDeleteRunsResultOutcome::SandboxPreserved,
            Some(response.sandbox),
        ),
        Err(error) => {
            let outcome = match error.status() {
                StatusCode::CONFLICT => BatchDeleteRunsResultOutcome::Conflict,
                _ => BatchDeleteRunsResultOutcome::Error,
            };
            BatchDeleteRunsResult {
                run_id: id.to_string(),
                ok: false,
                outcome,
                sandbox: None,
                error: Some(error.into_response_entry()),
            }
        }
    }
}

fn batch_delete_success(
    id: RunId,
    outcome: BatchDeleteRunsResultOutcome,
    sandbox: Option<DeleteRunSandbox>,
) -> BatchDeleteRunsResult {
    BatchDeleteRunsResult {
        run_id: id.to_string(),
        ok: true,
        outcome,
        sandbox,
        error: None,
    }
}

async fn batch_run_archive_item(
    state: &AppState,
    readiness: &AskFabroReadiness,
    actor: Principal,
    id: RunId,
    action: ArchiveAction,
) -> BatchRunLifecycleResult {
    let outcome = match run_archive_operation(state, &id, Some(actor), action).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let api_error = archive_workflow_error_to_api_error(err);
            let result_outcome = match api_error.status() {
                StatusCode::NOT_FOUND => BatchRunLifecycleResultOutcome::NotFound,
                StatusCode::CONFLICT => BatchRunLifecycleResultOutcome::Conflict,
                _ => BatchRunLifecycleResultOutcome::Error,
            };
            return batch_result_failure(id, result_outcome, api_error);
        }
    };

    match state.store.get_cached_summary(&id, Utc::now()).await {
        Ok(Some(summary)) => BatchRunLifecycleResult {
            run_id: id.to_string(),
            ok: true,
            outcome,
            run: Some(readiness.decorate(summary)),
            error: None,
        },
        Ok(None) => batch_result_failure(
            id,
            BatchRunLifecycleResultOutcome::Error,
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load run summary after lifecycle action.",
            ),
        ),
        Err(err) => batch_result_failure(
            id,
            BatchRunLifecycleResultOutcome::Error,
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        ),
    }
}

fn batch_result_failure(
    id: RunId,
    outcome: BatchRunLifecycleResultOutcome,
    error: ApiError,
) -> BatchRunLifecycleResult {
    BatchRunLifecycleResult {
        run_id: id.to_string(),
        ok: false,
        outcome,
        run: None,
        error: Some(error.into_response_entry()),
    }
}

async fn run_archive_operation(
    state: &AppState,
    id: &RunId,
    actor: Option<Principal>,
    action: ArchiveAction,
) -> Result<BatchRunLifecycleResultOutcome, WorkflowError> {
    match action {
        ArchiveAction::Archive => {
            operations::archive(&state.store, id, actor)
                .await
                .map(|outcome| match outcome {
                    operations::ArchiveOutcome::Archived { .. } => {
                        BatchRunLifecycleResultOutcome::Archived
                    }
                    operations::ArchiveOutcome::AlreadyArchived => {
                        BatchRunLifecycleResultOutcome::AlreadyArchived
                    }
                })
        }
        ArchiveAction::Unarchive => {
            operations::unarchive(&state.store, id, actor)
                .await
                .map(|outcome| match outcome {
                    operations::UnarchiveOutcome::Unarchived { .. } => {
                        BatchRunLifecycleResultOutcome::Unarchived
                    }
                    operations::UnarchiveOutcome::NotArchived { .. } => {
                        BatchRunLifecycleResultOutcome::NotArchived
                    }
                })
        }
    }
}

fn archive_workflow_error_to_api_error(err: WorkflowError) -> ApiError {
    match err {
        WorkflowError::Precondition(message) => ApiError::new(StatusCode::CONFLICT, message),
        WorkflowError::RunNotFound(_) => ApiError::not_found("Run not found."),
        err => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

async fn run_archive_action(
    state: Arc<AppState>,
    actor: Principal,
    id: RunId,
    action: ArchiveAction,
) -> Response {
    match run_archive_operation(state.as_ref(), &id, Some(actor), action).await {
        Ok(_) => archive_status_response(state.as_ref(), id).await,
        Err(err) => archive_workflow_error_to_api_error(err).into_response(),
    }
}

async fn archive_status_response(state: &AppState, id: RunId) -> Response {
    run_response(state, id, StatusCode::OK).await
}

/// Persist a synchronous pause/unpause transition: append the caller-supplied
/// events to the run store and mirror the new status in the in-memory run map.
/// Returns `Some(Response)` on error, `None` on success.
async fn synchronous_transition(
    state: &AppState,
    id: RunId,
    append_events: impl FnOnce(&mut Vec<workflow_event::Event>),
) -> Option<Response> {
    let run_store = match state.store.open_run(&id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            return Some(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
    };
    let mut events = Vec::new();
    append_events(&mut events);
    for event in events {
        if let Err(err) = workflow_event::append_event(&run_store, &id, &event).await {
            return Some(
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
            );
        }
        let stored = workflow_event::to_run_event(&id, &event);
        update_live_run_from_event(state, id, &stored);
    }
    None
}
