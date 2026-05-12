use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use fabro_api::types::SteerRunRequest;
use fabro_types::Principal;
use fabro_workflow::run_status::RunStatus;

use super::super::{
    AnswerTransportError, AppState, durable_run_status, parse_run_id_path, reject_if_archived,
};
use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;

pub(super) fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/runs/{id}/steer", post(steer_run))
        .route("/runs/{id}/interrupt", post(interrupt_run))
}

enum RunControlRequest {
    Steer { text: String },
    Interrupt,
    InterruptThenSteer { text: String },
}

impl RunControlRequest {
    const fn requires_active_api_session(&self) -> bool {
        matches!(self, Self::Interrupt | Self::InterruptThenSteer { .. })
    }
}

async fn steer_run(
    auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<SteerRunRequest>,
) -> Response {
    // OpenAPI enforces minLength=1/maxLength=8192 already; only whitespace-only
    // payloads can slip through.
    let SteerRunRequest { text, interrupt } = req;
    let text: String = text.into();
    if text.trim().is_empty() {
        return ApiError::bad_request("Steer text must not be empty.").into_response();
    }
    let control = if interrupt {
        RunControlRequest::InterruptThenSteer { text }
    } else {
        RunControlRequest::Steer { text }
    };

    control_run(auth, state, id, control).await
}

async fn interrupt_run(
    auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    control_run(auth, state, id, RunControlRequest::Interrupt).await
}

async fn control_run(
    auth: RequiredUser,
    state: Arc<AppState>,
    id: String,
    control: RunControlRequest,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }

    // Status + steerability gate. Take the answer_transport snapshot under
    // the same lock so we can hand it off without further state races.
    let managed_answer_transport = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => {
                match managed_run.status {
                    RunStatus::Blocked { .. } => {
                        return ApiError::with_code(
                            StatusCode::CONFLICT,
                            "Run is blocked on a question; use the interview-answer endpoint \
                             instead.",
                            "use_answer_endpoint",
                        )
                        .into_response();
                    }
                    RunStatus::Submitted
                    | RunStatus::Queued
                    | RunStatus::Starting
                    | RunStatus::Paused { .. } => {
                        return ApiError::with_code(
                            StatusCode::CONFLICT,
                            "Run is not currently running.",
                            "run_not_steerable",
                        )
                        .into_response();
                    }
                    RunStatus::Failed { .. }
                    | RunStatus::Succeeded { .. }
                    | RunStatus::Removing
                    | RunStatus::Dead => {
                        return terminal_control_response(&control);
                    }
                    RunStatus::Running => {}
                }
                // Steerability predicate. Best-effort, target-oriented:
                //   - If at least one API-mode session is active → forward.
                //   - Else if no agent stages are active at all → forward (worker hub buffers
                //     for the next session).
                //   - Else (active agents exist but all are non-steerable) → 409.
                if managed_run.active_api_stages.is_empty()
                    && !managed_run.active_non_steerable_agent_stages.is_empty()
                {
                    return ApiError::with_code(
                        StatusCode::CONFLICT,
                        "All currently running agent stages use a non-steerable backend.",
                        "agent_not_steerable",
                    )
                    .into_response();
                }
                if managed_run.active_api_stages.is_empty() && control.requires_active_api_session()
                {
                    return ApiError::with_code(
                        StatusCode::CONFLICT,
                        "Run has no active API-mode agent session.",
                        "no_active_api_session",
                    )
                    .into_response();
                }
                Some(managed_run.answer_transport.clone())
            }
            None => None,
        }
    };

    let Some(answer_transport) = managed_answer_transport else {
        return unmanaged_control_response(state.as_ref(), id, &control).await;
    };
    let Some(answer_transport) = answer_transport else {
        return ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "Run has no live worker control channel.",
            "worker_control_unavailable",
        )
        .into_response();
    };

    let actor = Principal::User(auth.0);
    let result = match control {
        RunControlRequest::Steer { text } => answer_transport.steer(text, actor).await,
        RunControlRequest::Interrupt => answer_transport.interrupt(actor).await,
        RunControlRequest::InterruptThenSteer { text } => {
            answer_transport.interrupt_then_steer(text, actor).await
        }
    };

    match result {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(AnswerTransportError::Timeout) => ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "Worker control channel timed out.",
            "worker_control_unavailable",
        )
        .into_response(),
        Err(AnswerTransportError::Closed) => ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "Worker control channel is closed.",
            "worker_control_unavailable",
        )
        .into_response(),
    }
}

fn terminal_control_response(control: &RunControlRequest) -> Response {
    let code = if matches!(control, RunControlRequest::Interrupt) {
        "run_not_interruptible"
    } else {
        "run_not_steerable"
    };
    ApiError::with_code(StatusCode::CONFLICT, "Run is no longer steerable.", code).into_response()
}

async fn unmanaged_control_response(
    state: &AppState,
    id: fabro_types::RunId,
    control: &RunControlRequest,
) -> Response {
    match durable_run_status(state, id).await {
        Ok(Some(status)) if status.is_terminal() => terminal_control_response(control),
        Ok(Some(_)) => ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "Run has no live worker control channel.",
            "worker_control_unavailable",
        )
        .into_response(),
        Ok(None) => ApiError::not_found("Run not found.").into_response(),
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}
