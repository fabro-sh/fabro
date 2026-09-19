//! The run pairing endpoints, which are not supported over Petri.
//!
//! A pair session was an Ask Fabro conversation bound to a live agent stage
//! through the legacy executor's steering hub, which the Petri run has no
//! adapter for. The status endpoint reports no pair and no target, and the
//! others refuse with `pair_unsupported`, so a client learns why rather than
//! waiting on a record that never lands. `fabro exec` and Ask Fabro sessions
//! are unaffected: they run on the Pebble builder directly.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use fabro_types::RunPairStatusResponse;

use super::super::AppState;
use crate::error::ApiError;
use crate::principal_middleware::RequireRunManagementTarget;

pub(super) fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/runs/{id}/pair",
            get(get_pair_status).post(pair_unsupported),
        )
        .route(
            "/runs/{id}/pair/{pair_id}",
            get(pair_unsupported).delete(pair_unsupported),
        )
        .route("/runs/{id}/pair/{pair_id}/messages", post(pair_unsupported))
        .route(
            "/runs/{id}/pair/{pair_id}/transcript",
            get(pair_unsupported),
        )
}

async fn get_pair_status(
    RequireRunManagementTarget(id, _actor): RequireRunManagementTarget,
    State(_state): State<Arc<AppState>>,
) -> Response {
    Json(RunPairStatusResponse {
        run_id:       id,
        current_pair: None,
        targets:      Vec::new(),
    })
    .into_response()
}

async fn pair_unsupported(
    RequireRunManagementTarget(_id, _actor): RequireRunManagementTarget,
    State(_state): State<Arc<AppState>>,
) -> Response {
    ApiError::with_code(
        StatusCode::NOT_IMPLEMENTED,
        "Pairing with a run's agent stage is not supported: the pair session ran through the \
         legacy executor, which Petri replaced. Use an Ask Fabro session on the run instead.",
        "pair_unsupported",
    )
    .into_response()
}
