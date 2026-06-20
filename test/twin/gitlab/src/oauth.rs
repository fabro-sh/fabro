use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde_json::json;

use crate::state::SharedGitLabState;

type AppState = (SharedGitLabState, String);

#[expect(
    clippy::disallowed_types,
    reason = "The GitLab twin parses caller-provided OAuth redirect URLs to append test callback parameters."
)]
pub(crate) async fn authorize(Query(query): Query<HashMap<String, String>>) -> Response {
    let Some(redirect_uri) = query.get("redirect_uri") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing redirect_uri" })),
        )
            .into_response();
    };

    let Ok(mut redirect) = url::Url::parse(redirect_uri) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid redirect_uri" })),
        )
            .into_response();
    };
    {
        let mut pairs = redirect.query_pairs_mut();
        pairs.append_pair("code", "gitlab-oauth-code");
        if let Some(state) = query.get("state") {
            pairs.append_pair("state", state);
        }
    }

    Redirect::temporary(redirect.as_str()).into_response()
}

pub(crate) async fn token(State((_state, automation_token)): State<AppState>) -> Response {
    Json(json!({
        "access_token": automation_token,
        "token_type": "Bearer",
        "expires_in": 7200,
        "refresh_token": "gitlab-oauth-refresh-token",
        "scope": "read_user",
        "created_at": 1_781_308_800u64,
    }))
    .into_response()
}
