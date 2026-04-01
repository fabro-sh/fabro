pub mod auth;
pub mod chat_completions;
pub mod models;
pub mod responses;

use axum::{Router, middleware, routing::post};

use crate::state::AppState;

pub fn router(require_auth: bool) -> Router<AppState> {
    let router = Router::new()
        .route("/responses", post(responses::create_response))
        .route(
            "/chat/completions",
            post(chat_completions::create_chat_completion),
        );

    if require_auth {
        router.layer(middleware::from_fn(auth::require_bearer_auth))
    } else {
        router
    }
}
