use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, header::RETRY_AFTER},
    response::IntoResponse,
};
use futures_util::future;
use tokio::time::{Duration, sleep};

use crate::{
    engine::{execute_responses_request, failures::ExecutionOutcome},
    openai::auth,
    sse::responses_sse_response,
    state::AppState,
};

use super::models::ResponsesRequest;

pub async fn create_response(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<ResponsesRequest>, JsonRejection>,
) -> impl IntoResponse {
    let namespace = match auth::openai_request_namespace(&headers, state.config.require_auth) {
        Ok(namespace) => namespace,
        Err(response) => return response,
    };

    let request = match payload {
        Ok(Json(request)) => request,
        Err(rejection) => {
            return super::models::OpenAiError::from_json_rejection(&rejection)
                .into_response()
                .into_response();
        }
    };

    match execute_responses_request(&state, &namespace, &request) {
        Ok(ExecutionOutcome::Success(success)) => {
            if success.transport.delay_before_headers_ms > 0 {
                sleep(Duration::from_millis(
                    success.transport.delay_before_headers_ms,
                ))
                .await;
            }

            if request.stream {
                responses_sse_response(&success.plan, success.transport).into_response()
            } else {
                Json(success.plan.responses_json()).into_response()
            }
        }
        Ok(ExecutionOutcome::Error(error)) => {
            if error.delay_before_headers_ms > 0 {
                sleep(Duration::from_millis(error.delay_before_headers_ms)).await;
            }

            let mut response = Json(error.body).into_response();
            *response.status_mut() = error.status;
            if let Some(retry_after) = error.retry_after {
                response.headers_mut().insert(
                    RETRY_AFTER,
                    retry_after.parse().expect("valid Retry-After header"),
                );
            }
            response
        }
        Ok(ExecutionOutcome::Hang {
            delay_before_headers_ms,
        }) => {
            if delay_before_headers_ms > 0 {
                sleep(Duration::from_millis(delay_before_headers_ms)).await;
            }
            future::pending::<()>().await;
            unreachable!()
        }
        Err(error) => error.into_response().into_response(),
    }
}
