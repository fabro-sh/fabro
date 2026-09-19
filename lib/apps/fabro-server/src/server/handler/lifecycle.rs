use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use fabro_store::platform_records::{PlatformRecord, RunLifecycleKind, RunLifecycleRecord};
use tokio::time::{Instant, sleep_until};

use super::super::{
    ApiError, AppState, AskFabroReadiness, BatchDeleteRunsRequest, BatchDeleteRunsResponse,
    BatchDeleteRunsResult, BatchDeleteRunsResultOutcome, BatchDeleteRunsSummary,
    BatchRunLifecycleRequest, BatchRunLifecycleResponse, BatchRunLifecycleResult,
    BatchRunLifecycleResultOutcome, BatchRunLifecycleSummary, DeleteRunOutcome, DeleteRunSandbox,
    DenyRunRequest, FailureReason, IntoResponse, Json, Path, PendingReason, Principal,
    RequireRunManagementTarget, RequiredUser, Response, Router, RunAnswerTransport,
    RunControlAction, RunExecutionMode, RunId, RunRunnableSource, RunStatus, StartRunRequest,
    State, StatusCode, Storage, WORKER_CANCEL_GRACE, WorkflowError, append_control_request,
    apply_lifecycle_to_managed_run, clear_live_run_state, delete_run_internal, durable_run_status,
    load_pending_control, managed_run, parse_run_id_path, persist_cancelled_run_status, post,
    reject_if_archived, run_records,
};
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
        .route("/runs/{id}/unarchive", post(unarchive_run))
}

pub(super) async fn run_response(state: &AppState, id: RunId, status: StatusCode) -> Response {
    match state.stores.run_summaries.get(&id, Utc::now()).await {
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

    let run_state = run_records::require_projection(state, id).await?;

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
    queue_run(state, id, &run_state, resume, actor).await
}

/// Queue the run for the scheduler: record that its start was requested
/// and that it is runnable (or pending approval), and register it as a
/// managed run in start or resume mode. The caller has checked that the run
/// may be queued; a fork, seeded to resume, is queued here without the
/// checkpoint check a resume of an interrupted run makes.
pub(super) async fn queue_run(
    state: &AppState,
    id: RunId,
    run_state: &fabro_store::RunProjection,
    resume: bool,
    actor: Principal,
) -> Result<(), ApiError> {
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
    let mut start_requested = RunLifecycleRecord::new(RunLifecycleKind::StartRequested);
    start_requested.source = Some(if resume { "resume" } else { "start" }.to_string());
    let next_status = if approval_required {
        RunStatus::Pending {
            reason: PendingReason::ApprovalRequired,
        }
    } else {
        RunStatus::Runnable
    };
    let next = if approval_required {
        run_records::transition(RunLifecycleKind::Pending, next_status)
    } else {
        runnable(RunRunnableSource::StartRequested)
    };
    for record in [start_requested, next] {
        if let Err(err) = run_records::lifecycle(state, id, record).await {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            ));
        }
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
    let run_state = match run_records::require_projection(state.as_ref(), id).await {
        Ok(run_state) => run_state,
        Err(err) => return err.into_response(),
    };
    if !matches!(run_state.status, RunStatus::Pending {
        reason: PendingReason::ApprovalRequired,
    }) {
        return ApiError::new(StatusCode::CONFLICT, "Run is not pending approval.").into_response();
    }
    let _ = user;

    for record in [
        RunLifecycleRecord::new(RunLifecycleKind::Approved),
        runnable(RunRunnableSource::Approved),
    ] {
        if let Err(err) = run_records::lifecycle(state.as_ref(), id, record).await {
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
    let run_state = match run_records::require_projection(state.as_ref(), id).await {
        Ok(run_state) => run_state,
        Err(err) => return err.into_response(),
    };
    if !matches!(run_state.status, RunStatus::Pending {
        reason: PendingReason::ApprovalRequired,
    }) {
        return ApiError::new(StatusCode::CONFLICT, "Run is not pending approval.").into_response();
    }
    let _ = user;

    let mut denied = RunLifecycleRecord::new(RunLifecycleKind::Denied);
    denied.reason.clone_from(&reason);
    for record in [
        denied,
        run_records::failed(FailureReason::ApprovalDenied, message.clone()),
    ] {
        if let Err(err) = run_records::lifecycle(state.as_ref(), id, record).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
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

fn schedule_worker_cancel_escalation(state: Arc<AppState>, run_id: RunId, worker_ref: WorkerRef) {
    let requested_at = Instant::now();
    let armed = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let Some(run) = runs.get_mut(&run_id) else {
            return;
        };
        if run.cancel_escalation_worker.as_ref() == Some(&worker_ref) {
            false
        } else {
            run.cancel_escalation_worker = Some(worker_ref.clone());
            true
        }
    };
    if !armed {
        tracing::debug!(
            run_id = %run_id,
            worker_kind = worker_ref.kind(),
            worker_ref = ?worker_ref,
            "Worker cancellation escalation is already armed"
        );
        return;
    }

    tokio::spawn(async move {
        sleep_until(requested_at + WORKER_CANCEL_GRACE).await;
        let should_escalate = {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let Some(run) = runs.get_mut(&run_id) else {
                return;
            };
            run.escalation_still_current(&worker_ref)
        };
        if !should_escalate {
            tracing::debug!(
                run_id = %run_id,
                worker_kind = worker_ref.kind(),
                worker_ref = ?worker_ref,
                "Skipping stale worker cancellation escalation"
            );
            return;
        }
        if !state.worker_runtime.is_alive(&worker_ref).await {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            if let Some(run) = runs.get_mut(&run_id) {
                run.clear_escalation_for(&worker_ref);
            }
            tracing::debug!(
                run_id = %run_id,
                worker_kind = worker_ref.kind(),
                worker_ref = ?worker_ref,
                "Skipping worker cancellation escalation because worker exited"
            );
            return;
        }
        let still_current = {
            let mut runs = state.runs.lock().expect("runs lock poisoned");
            let Some(run) = runs.get_mut(&run_id) else {
                return;
            };
            run.escalation_still_current(&worker_ref)
        };
        if !still_current {
            tracing::debug!(
                run_id = %run_id,
                worker_kind = worker_ref.kind(),
                worker_ref = ?worker_ref,
                "Skipping worker cancellation escalation after liveness check"
            );
            return;
        }

        let elapsed_ms = u64::try_from(requested_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::warn!(
            run_id = %run_id,
            worker_kind = worker_ref.kind(),
            worker_ref = ?worker_ref,
            elapsed_ms,
            "Force-stopping worker after cancellation grace period"
        );
        state.worker_runtime.force_stop(&worker_ref).await;
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(run) = runs.get_mut(&run_id) {
            run.clear_escalation_for(&worker_ref);
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
    let durable_summary = match state.stores.run_summaries.get(&id, Utc::now()).await {
        Ok(summary) => summary,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let pending_control = durable_summary
        .as_ref()
        .and_then(|summary| summary.lifecycle.pending_control);
    let durable_status = durable_summary
        .as_ref()
        .map(|summary| summary.lifecycle.status);
    let cancel_target = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get_mut(&id) {
            Some(managed_run) => {
                let managed_status = managed_run.status;
                match managed_status {
                    RunStatus::Submitted
                    | RunStatus::Pending { .. }
                    | RunStatus::Runnable
                    | RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. } => {
                        let answer_transport = managed_run.answer_transport.clone();
                        let should_cancel_pending_interview =
                            matches!(
                                &answer_transport,
                                Some(RunAnswerTransport::InProcess { .. })
                            ) && (matches!(managed_status, RunStatus::Blocked { .. })
                                || matches!(durable_status, Some(RunStatus::Blocked { .. })));
                        let persist_cancelled_status = matches!(
                            managed_status,
                            RunStatus::Submitted | RunStatus::Pending { .. } | RunStatus::Runnable
                        ) && !should_cancel_pending_interview;
                        if persist_cancelled_status {
                            managed_run.status = RunStatus::Failed {
                                reason: FailureReason::Cancelled,
                            };
                        }
                        let cancel_tx = if should_cancel_pending_interview {
                            None
                        } else {
                            managed_run.cancel_tx.take()
                        };
                        Some((
                            persist_cancelled_status,
                            answer_transport,
                            managed_run.cancel_token.clone(),
                            cancel_tx,
                            managed_run.worker_ref.clone(),
                        ))
                    }
                    _ => {
                        return ApiError::new(StatusCode::CONFLICT, "Run is not cancellable.")
                            .into_response();
                    }
                }
            }
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
    tracing::debug!(
        run_id = %id,
        delivered_control,
        "Processed cooperative run cancellation signal"
    );
    if let Some(worker_ref) = worker_ref {
        if !delivered_control {
            state.worker_runtime.request_stop(&worker_ref).await;
        }
        schedule_worker_cancel_escalation(Arc::clone(&state), id, worker_ref);
    }

    let response_status = if persist_cancelled_status {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), id).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };

    run_response(state.as_ref(), id, response_status).await
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
            if let Some(response) = synchronous_transition(
                state.as_ref(),
                id,
                RunLifecycleRecord::new(RunLifecycleKind::Paused),
            )
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
            if let Some(response) = synchronous_transition(
                state.as_ref(),
                id,
                RunLifecycleRecord::new(RunLifecycleKind::Unpaused),
            )
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
    Box::pin(batch_run_archive_action(
        state,
        Principal::User(user),
        request,
        ArchiveAction::Archive,
    ))
    .await
}

async fn batch_unarchive_runs(
    RequiredUser(user): RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(request): Json<BatchRunLifecycleRequest>,
) -> Response {
    Box::pin(batch_run_archive_action(
        state,
        Principal::User(user),
        request,
        ArchiveAction::Unarchive,
    ))
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

#[derive(Clone, Copy)]
pub(super) enum ArchiveAction {
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

    match state.stores.run_summaries.get(&id, Utc::now()).await {
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

/// Archive a terminal run, or unarchive one: idempotent either way, refused
/// with a precondition error when the run is not terminal.
pub(super) async fn run_archive_operation(
    state: &AppState,
    id: &RunId,
    actor: Option<Principal>,
    action: ArchiveAction,
) -> Result<BatchRunLifecycleResultOutcome, WorkflowError> {
    let _ = actor;
    let projection = run_records::projection(state, *id)
        .await
        .map_err(|err| WorkflowError::engine(err.to_string()))?
        .ok_or_else(|| WorkflowError::RunNotFound(id.to_string()))?;
    let current = projection.status;
    let terminal = matches!(
        current,
        RunStatus::Succeeded { .. } | RunStatus::Failed { .. } | RunStatus::Dead
    );
    let archived = projection.archived_at.is_some();
    let record = match action {
        ArchiveAction::Archive if archived => {
            return Ok(BatchRunLifecycleResultOutcome::AlreadyArchived);
        }
        ArchiveAction::Archive if !terminal => {
            return Err(WorkflowError::Precondition(format!(
                "run {id} must be terminal (succeeded, failed, or dead) to archive; current \
                 status is {current}"
            )));
        }
        ArchiveAction::Archive => PlatformRecord::RunArchived,
        ArchiveAction::Unarchive if archived => PlatformRecord::RunUnarchived,
        ArchiveAction::Unarchive if terminal => {
            return Ok(BatchRunLifecycleResultOutcome::NotArchived);
        }
        ArchiveAction::Unarchive => {
            return Err(WorkflowError::Precondition(format!(
                "run {id} is not archived (status: {current}); nothing to unarchive"
            )));
        }
    };
    let outcome = match action {
        ArchiveAction::Archive => BatchRunLifecycleResultOutcome::Archived,
        ArchiveAction::Unarchive => BatchRunLifecycleResultOutcome::Unarchived,
    };
    run_records::append(state, *id, record)
        .await
        .map_err(|err| WorkflowError::engine(err.to_string()))?;
    Ok(outcome)
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

/// The runnable transition, with what made the run runnable.
fn runnable(source: RunRunnableSource) -> RunLifecycleRecord {
    let mut record = run_records::transition(RunLifecycleKind::Runnable, RunStatus::Runnable);
    record.source = Some(<&'static str>::from(source).to_string());
    record
}

/// Persist a synchronous pause/unpause transition: record it and mirror the
/// new status in the in-memory run map. Returns `Some(Response)` on error,
/// `None` on success.
async fn synchronous_transition(
    state: &AppState,
    id: RunId,
    record: RunLifecycleRecord,
) -> Option<Response> {
    if let Err(err) = run_records::lifecycle(state, id, record.clone()).await {
        return Some(
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        );
    }
    apply_lifecycle_to_managed_run(state, id, &record);
    None
}
