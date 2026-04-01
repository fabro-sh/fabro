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
    use crate::state::{AppConfig, AppState};

    fn test_rsa_key() -> String {
        use std::process::Command;
        let output = Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .output()
            .expect("openssl should be available");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    fn sign_test_jwt(app_id: &str, private_key_pem: &str) -> String {
        use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
        use serde::Serialize;

        #[derive(Serialize)]
        struct Claims {
            iss: String,
            iat: i64,
            exp: i64,
        }

        let now = chrono::Utc::now().timestamp();
        let claims = Claims {
            iss: app_id.to_string(),
            iat: now - 60,
            exp: now + 600,
        };
        let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).unwrap();
        encode(&Header::new(Algorithm::RS256), &claims, &key).unwrap()
    }

    #[tokio::test]
    async fn get_app_returns_app_info() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppConfig {
            app_id: "12345".to_string(),
            slug: "my-app".to_string(),
            owner_login: "my-org".to_string(),
            public: true,
            private_key_pem: pem.clone(),
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("12345", &pem);
        let client = reqwest::Client::new();
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
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppConfig {
            app_id: "12345".to_string(),
            slug: "my-app".to_string(),
            owner_login: "my-org".to_string(),
            public: true,
            private_key_pem: pem,
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let client = reqwest::Client::new();
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
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppConfig {
            app_id: "12345".to_string(),
            slug: "my-app".to_string(),
            owner_login: "my-org".to_string(),
            public: true,
            private_key_pem: pem,
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let resp = reqwest::get(&format!("{}/apps/my-app", server.url()))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let resp404 = reqwest::get(&format!("{}/apps/nonexistent", server.url()))
            .await
            .unwrap();
        assert_eq!(resp404.status(), 404);

        server.shutdown().await;
    }

    #[tokio::test]
    async fn get_apps_slug_private_returns_404() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppConfig {
            app_id: "12345".to_string(),
            slug: "private-app".to_string(),
            owner_login: "my-org".to_string(),
            public: false,
            private_key_pem: pem,
            webhook_secret: None,
        });
        let server = TestServer::start(state).await;

        let resp = reqwest::get(&format!("{}/apps/private-app", server.url()))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        server.shutdown().await;
    }
}
