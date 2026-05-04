use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use fabro_api::types::SteerRunRequest;
use fabro_types::{Principal, SteerKind};
use fabro_workflow::run_status::RunStatus;

use super::super::{AnswerTransportError, AppState, parse_run_id_path, reject_if_archived};
use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;

pub(super) fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/runs/{id}/steer", post(steer_run))
}

const MAX_STEER_TEXT_LEN: usize = 8192;

async fn steer_run(
    auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<SteerRunRequest>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }

    // Body validation. (OpenAPI enforces minLength=1/maxLength=8192 at the
    // type boundary already; we re-check defensively for trims/whitespace.)
    let text = req.text.to_string();
    let trimmed_len = text.trim().len();
    if trimmed_len == 0 {
        return ApiError::bad_request("Steer text must not be empty.").into_response();
    }
    if text.len() > MAX_STEER_TEXT_LEN {
        return ApiError::bad_request(format!(
            "Steer text must be ≤ {MAX_STEER_TEXT_LEN} characters."
        ))
        .into_response();
    }
    let kind = if req.interrupt {
        SteerKind::Interrupt
    } else {
        SteerKind::Append
    };

    // Status + steerability gate. Take the answer_transport snapshot under
    // the same lock so we can hand it off without further state races.
    let answer_transport = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        let Some(managed_run) = runs.get(&id) else {
            return ApiError::not_found("Run not found.").into_response();
        };
        match managed_run.status {
            RunStatus::Blocked { .. } => {
                return ApiError::with_code(
                    StatusCode::CONFLICT,
                    "Run is blocked on a question; use the interview-answer endpoint instead.",
                    "use_answer_endpoint",
                )
                .into_response();
            }
            RunStatus::Submitted
            | RunStatus::Queued
            | RunStatus::Starting
            | RunStatus::Paused { .. } => {
                return ApiError::new(StatusCode::CONFLICT, "Run is not currently running.")
                    .into_response();
            }
            RunStatus::Failed { .. }
            | RunStatus::Succeeded { .. }
            | RunStatus::Removing
            | RunStatus::Dead
            | RunStatus::Archived { .. } => {
                return ApiError::new(StatusCode::CONFLICT, "Run is no longer steerable.")
                    .into_response();
            }
            RunStatus::Running => {}
        }
        // Steerability predicate. Best-effort, target-oriented:
        //   - If at least one API-mode session is active → forward.
        //   - Else if no agent stages are active at all → forward (worker hub buffers
        //     for the next session).
        //   - Else (active agents exist but all are CLI-mode) → 409.
        if managed_run.active_api_stages.is_empty() && !managed_run.active_cli_stages.is_empty() {
            return ApiError::with_code(
                StatusCode::CONFLICT,
                "All currently running agent stages are CLI-mode and cannot be steered.",
                "cli_agent_not_steerable",
            )
            .into_response();
        }
        managed_run.answer_transport.clone()
    };

    let Some(answer_transport) = answer_transport else {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Run has no live worker control channel.",
        )
        .into_response();
    };

    let actor = Principal::User(auth.0);
    match answer_transport.steer(text, kind, actor).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(AnswerTransportError::Timeout) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Worker control channel timed out.",
        )
        .into_response(),
        Err(AnswerTransportError::Closed) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Worker control channel is closed.",
        )
        .into_response(),
    }
}
