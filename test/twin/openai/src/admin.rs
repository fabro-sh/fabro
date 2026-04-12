use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::engine::scenario::ScenarioEnvelope;
use crate::openai::auth;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/__admin/scenarios", post(load_scenarios))
        .route("/__admin/requests", get(request_logs))
        .route("/__admin/reset", post(reset))
}

async fn load_scenarios(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ScenarioEnvelope>,
) -> Response {
    let namespace = match auth::admin_request_namespace(&headers) {
        Ok(namespace) => namespace,
        Err(response) => return response,
    };

    state.enqueue_scenarios(&namespace, payload.scenarios);
    Json(json!({ "status": "ok" })).into_response()
}

async fn request_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let namespace = match auth::admin_request_namespace(&headers) {
        Ok(namespace) => namespace,
        Err(response) => return response,
    };

    Json(json!({ "requests": state.request_logs(&namespace) })).into_response()
}

async fn reset(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let namespace = match auth::admin_request_namespace(&headers) {
        Ok(namespace) => namespace,
        Err(response) => return response,
    };

    state.reset(&namespace);
    Json(json!({ "status": "ok" })).into_response()
}
