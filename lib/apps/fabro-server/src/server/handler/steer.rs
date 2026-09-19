use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use fabro_api::types::{
    InterruptRunRequest, RunControlAcknowledgement, RunControlOutcome, SteerRunRequest,
};
use fabro_types::Principal;
use fabro_workflow::run_status::RunStatus;

use super::super::{
    AnswerTransportError, AppState, RunControlAnswer, durable_run_status, reject_if_archived,
};
use crate::error::ApiError;
use crate::principal_middleware::RequireRunManagementTarget;

pub(super) fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/runs/{id}/steer", post(steer_run))
        .route("/runs/{id}/interrupt", post(interrupt_run))
}

/// A control forwarded to the run's worker. The worker resolves the stage
/// and delivers the control to Petri, and answers over its control stream:
/// the answer is this endpoint's response, 202 once delivered, 409 with
/// the refusal's code when refused, and 202 `pending` when no answer came
/// within the wait. What the worker cannot deliver it also refuses on the
/// run's stream as a `run.notice` whose code says why.
enum RunControlRequest {
    /// Guidance for a live agent stage's session, run as a follow-up turn.
    Steer {
        text:  String,
        stage: Option<String>,
    },
    /// Stop a live agent stage's current model turn and keep its session;
    /// `text`, when given, is the stage's next input.
    Interrupt {
        stage: Option<String>,
        text:  Option<String>,
    },
}

impl RunControlRequest {
    fn name(&self) -> &'static str {
        match self {
            Self::Steer { .. } => "steer",
            Self::Interrupt { .. } => "interrupt",
        }
    }
}

/// A stage name, when given, must not be blank.
fn blank_stage(stage: Option<&str>) -> bool {
    stage.is_some_and(|stage| stage.trim().is_empty())
}

async fn steer_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    Json(req): Json<SteerRunRequest>,
) -> Response {
    // OpenAPI enforces minLength=1/maxLength=8192 already; only whitespace-only
    // payloads can slip through.
    let SteerRunRequest {
        text,
        interrupt,
        stage,
    } = req;
    let text: String = text.into();
    if text.trim().is_empty() {
        return ApiError::bad_request("Steer text must not be empty.").into_response();
    }
    let stage = stage.map(String::from);
    if blank_stage(stage.as_deref()) {
        return ApiError::bad_request("Steer stage must not be empty.").into_response();
    }
    let control = if interrupt {
        RunControlRequest::Interrupt {
            stage,
            text: Some(text),
        }
    } else {
        RunControlRequest::Steer { text, stage }
    };
    control_run(actor, state, id, control).await
}

/// Stop a live agent stage's current model turn. The body is optional: no
/// body interrupts the run's one live agent stage and leaves it waiting
/// for the next steer.
async fn interrupt_run(
    RequireRunManagementTarget(id, actor): RequireRunManagementTarget,
    State(state): State<Arc<AppState>>,
    body: Option<Json<InterruptRunRequest>>,
) -> Response {
    let InterruptRunRequest { stage, text } = body.map(|Json(body)| body).unwrap_or_default();
    let stage = stage.map(String::from);
    if blank_stage(stage.as_deref()) {
        return ApiError::bad_request("Interrupt stage must not be empty.").into_response();
    }
    let text = text.map(String::from);
    if text.as_deref().is_some_and(|text| text.trim().is_empty()) {
        return ApiError::bad_request("Interrupt text must not be empty.").into_response();
    }
    control_run(actor, state, id, RunControlRequest::Interrupt {
        stage,
        text,
    })
    .await
}

async fn control_run(
    actor: Principal,
    state: Arc<AppState>,
    id: fabro_types::RunId,
    control: RunControlRequest,
) -> Response {
    if let Some(response) = reject_if_archived(state.as_ref(), &id).await {
        return response;
    }

    // Status + steerability gate. Take the answer_transport snapshot under
    // the same lock so we can hand it off without further state races.
    let managed_answer_transport = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        match runs.get(&id) {
            Some(managed_run) => {
                match (&control, &managed_run.status) {
                    // A blocked run may still have an agent stage running a
                    // turn beside the question; the worker judges the
                    // interrupt per stage and refuses the gate itself.
                    (RunControlRequest::Interrupt { .. }, RunStatus::Blocked { .. })
                    | (_, RunStatus::Running) => {}
                    (RunControlRequest::Steer { .. }, RunStatus::Blocked { .. }) => {
                        return ApiError::with_code(
                            StatusCode::CONFLICT,
                            "Run is blocked on a question; use the interview-answer endpoint \
                             instead.",
                            "use_answer_endpoint",
                        )
                        .into_response();
                    }
                    (
                        _,
                        RunStatus::Submitted
                        | RunStatus::Pending { .. }
                        | RunStatus::Runnable
                        | RunStatus::Starting
                        | RunStatus::Paused { .. },
                    ) => {
                        return ApiError::with_code(
                            StatusCode::CONFLICT,
                            "Run is not currently running.",
                            not_controllable_code(&control),
                        )
                        .into_response();
                    }
                    (
                        _,
                        RunStatus::Failed { .. }
                        | RunStatus::Succeeded { .. }
                        | RunStatus::Removing
                        | RunStatus::Dead,
                    ) => {
                        return terminal_control_response(&control);
                    }
                }
                // Plain steers buffer in the worker hub when no agent session
                // is active; if active agents exist but none are steerable,
                // there is no live control channel to target.
                if managed_run.active_steerable_stages.is_empty()
                    && !managed_run.active_non_steerable_stages.is_empty()
                {
                    return ApiError::with_code(
                        StatusCode::CONFLICT,
                        "All currently running agent stages use a non-steerable backend.",
                        "agent_not_steerable",
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

    let result = match control {
        RunControlRequest::Steer { text, stage } => {
            answer_transport.steer(text, stage, actor).await
        }
        RunControlRequest::Interrupt { stage, text } => {
            answer_transport.interrupt(stage, text, actor).await
        }
    };

    match result {
        Ok(RunControlAnswer::Delivered { stage }) => accepted(RunControlOutcome::Delivered, stage),
        Ok(RunControlAnswer::Pending) => accepted(RunControlOutcome::Pending, None),
        Ok(RunControlAnswer::Refused { code, message }) => {
            ApiError::with_code(StatusCode::CONFLICT, message, code).into_response()
        }
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

/// The 202 of a control the worker took: delivered to `stage`, or still
/// pending its answer.
fn accepted(outcome: RunControlOutcome, stage: Option<String>) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(RunControlAcknowledgement { outcome, stage }),
    )
        .into_response()
}

/// The 409 code of a control the run's status refuses.
fn not_controllable_code(control: &RunControlRequest) -> &'static str {
    match control {
        RunControlRequest::Steer { .. } => "run_not_steerable",
        RunControlRequest::Interrupt { .. } => "run_not_interruptible",
    }
}

fn terminal_control_response(control: &RunControlRequest) -> Response {
    ApiError::with_code(
        StatusCode::CONFLICT,
        format!("Run no longer accepts a {}.", control.name()),
        not_controllable_code(control),
    )
    .into_response()
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
