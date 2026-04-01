use axum::{Json, Router, response::IntoResponse, routing::get};
use serde_json::json;

use crate::{admin, debug_ui, openai, state::AppState};

pub fn router(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", openai::router(state.config.require_auth));

    if state.config.enable_admin {
        router = router.merge(admin::router()).merge(debug_ui::router());
    }

    router.with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}
