use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::state::AppState;
use crate::{admin, debug_ui, openai};

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
