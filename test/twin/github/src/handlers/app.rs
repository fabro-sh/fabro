use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::auth::{extract_bearer_token, verify_app_jwt};
use crate::server::SharedState;

/// GET /app — returns authenticated app info
pub async fn get_app(State(state): State<SharedState>, headers: HeaderMap) -> impl IntoResponse {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"message": "Unauthorized"})),
            )
                .into_response();
        }
    };

    let state = state.read().await;

    // Try verifying the JWT against each registered app's public key
    for app in state.apps.values() {
        if let Ok(app_id) = verify_app_jwt(&token, &app.public_key_pem) {
            if app_id == app.config.app_id {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "slug": app.config.slug,
                        "owner": { "login": app.config.owner_login },
                    })),
                )
                    .into_response();
            }
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"message": "Unauthorized"})),
    )
        .into_response()
}

/// GET /apps/{slug} — check if app is public
pub async fn get_app_by_slug(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
) -> impl IntoResponse {
    let state = state.read().await;

    for app in state.apps.values() {
        if app.config.slug == slug && app.config.public {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "slug": app.config.slug,
                    "owner": { "login": app.config.owner_login },
                })),
            )
                .into_response();
        }
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"message": "Not Found"})),
    )
        .into_response()
}

/// PATCH /app/hook/config — update webhook configuration
pub async fn patch_webhook_config(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => return StatusCode::UNAUTHORIZED,
    };

    let mut state = state.write().await;

    // Verify JWT
    let mut authenticated = false;
    for app in state.apps.values() {
        if let Ok(app_id) = verify_app_jwt(&token, &app.public_key_pem) {
            if app_id == app.config.app_id {
                authenticated = true;
                break;
            }
        }
    }

    if !authenticated {
        return StatusCode::UNAUTHORIZED;
    }

    if let Some(url) = body.get("url").and_then(|v| v.as_str()) {
        state.webhook_config.url = Some(url.to_string());
    }
    if let Some(ct) = body.get("content_type").and_then(|v| v.as_str()) {
        state.webhook_config.content_type = Some(ct.to_string());
    }

    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use crate::server::TestServer;
    use crate::state::{AppOptions, AppState};
    use crate::test_support::{sign_test_jwt, test_http_client, test_rsa_private_key};

    #[tokio::test]
    async fn get_app_returns_app_info() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "12345".to_string(),
            slug: "my-app".to_string(),
            owner_login: "my-org".to_string(),
            public: true,
            private_key_pem: pem.to_string(),
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("12345", pem);
        let client = test_http_client();
        let resp = client
            .get(&format!("{}/app", server.url()))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["slug"], "my-app");
        assert_eq!(body["owner"]["login"], "my-org");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn get_app_rejects_invalid_jwt() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "12345".to_string(),
            slug: "my-app".to_string(),
            owner_login: "my-org".to_string(),
            public: true,
            private_key_pem: pem.to_string(),
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let client = test_http_client();
        let resp = client
            .get(&format!("{}/app", server.url()))
            .header("Authorization", "Bearer invalid-jwt")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn get_apps_slug_public() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "12345".to_string(),
            slug: "my-app".to_string(),
            owner_login: "my-org".to_string(),
            public: true,
            private_key_pem: pem.to_string(),
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let resp = test_http_client()
            .get(format!("{}/apps/my-app", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let resp404 = test_http_client()
            .get(format!("{}/apps/nonexistent", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp404.status(), 404);

        server.shutdown().await;
    }

    #[tokio::test]
    async fn get_apps_slug_private_returns_404() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "12345".to_string(),
            slug: "private-app".to_string(),
            owner_login: "my-org".to_string(),
            public: false,
            private_key_pem: pem.to_string(),
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let resp = test_http_client()
            .get(format!("{}/apps/private-app", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        server.shutdown().await;
    }
}
