use std::sync::Arc;

use super::super::{
    ApiError, AppState, FailureReason, ForkRequest, ForkResponse, IntoResponse, Json, Path,
    Principal, RequireRunScopedOrRunTools, RequiredUser, Response, RewindRequest, RewindResponse,
    Router, RunAnswerTransport, RunControlAction, RunExecutionMode, RunId, RunStatus,
    StartRunRequest, State, StatusCode, Storage, TimelineEntryResponse, WORKER_CANCEL_GRACE,
    WorkflowError, append_control_request, durable_run_status, get, load_pending_control,
    managed_run, operations, parse_run_id_path, persist_cancelled_run_status, post,
    reject_if_archived, sleep, update_live_run_from_event, workflow_event,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/start", post(start_run))
        .route("/runs/{id}/pause", post(pause_run))
        .route("/runs/{id}/unpause", post(unpause_run))
        .route("/runs/{id}/archive", post(archive_run))
        .route("/runs/{id}/rewind", post(rewind_run))
        .route("/runs/{id}/fork", post(fork_run))
        .route("/runs/{id}/timeline", get(run_timeline))
        .route("/runs/{id}/unarchive", post(unarchive_run))
}

async fn run_response(state: &AppState, id: RunId, status: StatusCode) -> Response {
    match state.store.get_cached_summary(&id).await {
        Ok(Some(summary)) => (status, Json(summary)).into_response(),
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn start_run(
    RequireRunScopedOrRunTools(id, _actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
    body: Option<Json<StartRunRequest>>,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }
    let resume = body.is_some_and(|Json(req)| req.resume);

    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs.get(&id) {
            if matches!(
                managed_run.status,
                RunStatus::Queued
                    | RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. }
            ) {
                return ApiError::new(
                    StatusCode::CONFLICT,
                    if resume {
                        "an engine process is still running for this run — cannot resume"
                    } else {
                        "an engine process is still running for this run — cannot start"
                    },
                )
                .into_response();
            }
        }
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

    if resume {
        if run_state.current_checkpoint().is_none() {
            return ApiError::new(StatusCode::CONFLICT, "no checkpoint to resume from")
                .into_response();
        }
    } else {
        let status = run_state.status;
        if !matches!(
            status,
            RunStatus::Submitted | RunStatus::Queued | RunStatus::Starting
        ) {
            return ApiError::new(
                StatusCode::CONFLICT,
                format!("cannot start run: status is {status}, expected submitted"),
            )
            .into_response();
        }
    }

    let run_dir = Storage::new(state.server_storage_dir())
        .run_scratch(&id)
        .root()
        .to_path_buf();
    let dot_source = run_state.spec.graph_source.clone().unwrap_or_default();
    if let Err(err) =
        workflow_event::append_event(&run_store, &id, &workflow_event::Event::RunQueued).await
    {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            id,
            managed_run(
                dot_source,
                RunStatus::Queued,
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

    state.scheduler_notify.notify_one();
    run_response(state.as_ref(), id, StatusCode::OK).await
}

fn schedule_worker_kill(state: Arc<AppState>, run_id: RunId, worker_pid: u32) {
    tokio::spawn(async move {
        sleep(WORKER_CANCEL_GRACE).await;
        let current_pid = {
            let runs = state.runs.lock().expect("runs lock poisoned");
            runs.get(&run_id).and_then(|run| run.worker_pid)
        };
        if current_pid == Some(worker_pid) && fabro_proc::process_group_alive(worker_pid) {
            #[cfg(unix)]
            fabro_proc::sigkill_process_group(worker_pid);
        }
    });
}

async fn cancel_run(
    RequireRunScopedOrRunTools(id, actor): RequireRunScopedOrRunTools,
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
                | RunStatus::Queued
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Blocked { .. }
                | RunStatus::Paused { .. } => {
                    let use_cancel_signal = !matches!(
                        managed_run.answer_transport,
                        Some(RunAnswerTransport::InProcess { .. })
                    );
                    let persist_cancelled_status =
                        matches!(managed_run.status, RunStatus::Submitted | RunStatus::Queued);
                    if persist_cancelled_status {
                        managed_run.status = RunStatus::Failed {
                            reason: FailureReason::Cancelled,
                        };
                    }
                    Some((
                        persist_cancelled_status,
                        managed_run.answer_transport.clone(),
                        managed_run.cancel_token.clone(),
                        use_cancel_signal
                            .then(|| managed_run.cancel_tx.take())
                            .flatten(),
                        managed_run.worker_pid,
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
    let Some((persist_cancelled_status, answer_transport, cancel_token, cancel_tx, worker_pid)) =
        cancel_target
    else {
        return unmanaged_cancel_response(state.as_ref(), id).await;
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
    let sent_cancel_signal = if let Some(cancel_tx) = cancel_tx {
        let _ = cancel_tx.send(());
        true
    } else {
        false
    };
    if let Some(answer_transport) = answer_transport {
        if !(sent_cancel_signal && matches!(answer_transport, RunAnswerTransport::InProcess { .. }))
        {
            let _ = answer_transport.cancel_run().await;
        }
    }
    if let Some(worker_pid) = worker_pid {
        #[cfg(unix)]
        fabro_proc::sigterm(worker_pid);
        schedule_worker_kill(Arc::clone(&state), id, worker_pid);
    }

    if persist_cancelled_status {
        if let Err(err) = persist_cancelled_run_status(state.as_ref(), id).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    run_response(state.as_ref(), id, StatusCode::OK).await
}

async fn unmanaged_cancel_response(state: &AppState, id: RunId) -> Response {
    match durable_run_status(state, id).await {
        Ok(Some(status)) if status.is_terminal() => ApiError::new(
            StatusCode::CONFLICT,
            "Run is already terminal and cannot be cancelled.",
        )
        .into_response(),
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
    /// Worker is running; ask it to pause via SIGUSR1. Status flips to
    /// `Paused` once the worker acknowledges.
    Signal { worker_pid: u32 },
    /// Worker is blocked on a human gate; flip to `Paused` directly by
    /// appending `RunPaused` ourselves.
    AppendEvent,
}

/// How `unpause_run` should enact the transition.
enum UnpauseMode {
    /// No outstanding block; ask the worker to resume via SIGUSR2.
    Signal { worker_pid: u32 },
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
                let Some(worker_pid) = managed_run.worker_pid else {
                    return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.")
                        .into_response();
                };
                PauseMode::Signal { worker_pid }
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
        PauseMode::Signal { worker_pid } => {
            #[cfg(unix)]
            fabro_proc::sigusr1(worker_pid);
            #[cfg(not(unix))]
            let _ = worker_pid;
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
                    let Some(worker_pid) = managed_run.worker_pid else {
                        return ApiError::new(StatusCode::CONFLICT, "Run worker is not available.")
                            .into_response();
                    };
                    UnpauseMode::Signal { worker_pid }
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
        UnpauseMode::Signal { worker_pid } => {
            #[cfg(unix)]
            fabro_proc::sigusr2(worker_pid);
            #[cfg(not(unix))]
            let _ = worker_pid;
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
    RequireRunScopedOrRunTools(id, actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
) -> Response {
    run_archive_action(state, actor, id, ArchiveAction::Archive).await
}

async fn unarchive_run(
    RequireRunScopedOrRunTools(id, actor): RequireRunScopedOrRunTools,
    State(state): State<Arc<AppState>>,
) -> Response {
    run_archive_action(state, actor, id, ArchiveAction::Unarchive).await
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

async fn run_archive_action(
    state: Arc<AppState>,
    actor: Principal,
    id: RunId,
    action: ArchiveAction,
) -> Response {
    let actor = Some(actor);
    let result = match action {
        ArchiveAction::Archive => operations::archive(&state.store, &id, actor)
            .await
            .map(|_| ()),
        ArchiveAction::Unarchive => operations::unarchive(&state.store, &id, actor)
            .await
            .map(|_| ()),
    };
    match result {
        Ok(()) => archive_status_response(state.as_ref(), id).await,
        Err(WorkflowError::Precondition(message)) => {
            ApiError::new(StatusCode::CONFLICT, message).into_response()
        }
        Err(WorkflowError::RunNotFound(_)) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
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
