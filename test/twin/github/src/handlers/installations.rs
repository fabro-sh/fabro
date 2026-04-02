use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::auth::{extract_bearer_token, verify_app_jwt};
use crate::server::SharedState;

/// GET /repos/{owner}/{repo}/installation
pub async fn get_installation(
    State(state): State<SharedState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
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

    // Verify JWT against registered apps
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"message": "Unauthorized"})),
        )
            .into_response();
    }

    match state.find_installation(&owner, &repo) {
        Some(installation) if installation.suspended => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "message": "GitHub App installation is suspended"
            })),
        )
            .into_response(),
        Some(installation) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "id": installation.id,
                "app_id": installation.app_id,
                "account": {
                    "login": installation.owner,
                },
            })),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "message": "Not Found"
            })),
        )
            .into_response(),
    }
}

/// POST /app/installations/{id}/access_tokens
pub async fn create_access_token(
    State(state): State<SharedState>,
    Path(id): Path<u64>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
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

    let mut state = state.write().await;

    // Verify JWT
    let mut authenticated_app_id: Option<String> = None;
    for app in state.apps.values() {
        if let Ok(app_id) = verify_app_jwt(&token, &app.public_key_pem) {
            if app_id == app.config.app_id {
                authenticated_app_id = Some(app_id);
                break;
            }
        }
    }

    let app_id = match authenticated_app_id {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"message": "Unauthorized"})),
            )
                .into_response();
        }
    };

    let installation = match state.find_installation_by_id(id) {
        Some(i) => i.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"message": "Not Found"})),
            )
                .into_response();
        }
    };

    // Validate requested repos are in installation's repo list
    let requested_repos: Vec<String> = body
        .get("repositories")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let permissions = body
        .get("permissions")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    if !requested_repos.is_empty() {
        for repo in &requested_repos {
            if !installation.repositories.contains(repo) {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "message": format!("Repository '{}' is not part of this installation", repo)
                    })),
                )
                    .into_response();
            }
        }
    }

    let repos_for_token = if requested_repos.is_empty() {
        installation.repositories.clone()
    } else {
        requested_repos
    };

    let access_token = state.generate_access_token(&app_id, id, repos_for_token, permissions);

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "token": access_token,
            "expires_at": "2099-01-01T00:00:00Z",
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use crate::server::TestServer;
    use crate::state::{AppOptions, AppState};

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
    async fn get_installation_returns_id() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.clone(),
            webhook_secret: None,
        });
        state.add_installation("100", "owner", vec!["repo".to_string()], false);
        state.add_repository("owner", "repo", vec!["main".to_string()], false);
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("100", &pem);
        let client = reqwest::Client::new();
        let resp = client
            .get(&format!("{}/repos/owner/repo/installation", server.url()))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "test-agent")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["id"].as_u64().is_some());
    }

    #[tokio::test]
    async fn get_installation_returns_404_when_not_installed() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.clone(),
            webhook_secret: None,
        });
        // No installation added
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("100", &pem);
        let client = reqwest::Client::new();
        let resp = client
            .get(&format!("{}/repos/owner/repo/installation", server.url()))
            .header("Authorization", format!("Bearer {jwt}"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn get_installation_returns_403_when_suspended() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.clone(),
            webhook_secret: None,
        });
        state.add_installation("100", "owner", vec!["repo".to_string()], true); // suspended
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("100", &pem);
        let client = reqwest::Client::new();
        let resp = client
            .get(&format!("{}/repos/owner/repo/installation", server.url()))
            .header("Authorization", format!("Bearer {jwt}"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 403);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn create_access_token_returns_201() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.clone(),
            webhook_secret: None,
        });
        let install_id = state.add_installation("100", "owner", vec!["repo".to_string()], false);
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("100", &pem);
        let client = reqwest::Client::new();
        let resp = client
            .post(&format!(
                "{}/app/installations/{install_id}/access_tokens",
                server.url()
            ))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .json(&serde_json::json!({
                "repositories": ["repo"],
                "permissions": {"contents": "write"}
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        let token = body["token"].as_str().unwrap();
        assert!(token.starts_with("ghs_"));
    }

    #[tokio::test]
    async fn create_access_token_returns_422_for_unauthorized_repo() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.clone(),
            webhook_secret: None,
        });
        let install_id = state.add_installation("100", "owner", vec!["repo".to_string()], false);
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("100", &pem);
        let client = reqwest::Client::new();
        let resp = client
            .post(&format!(
                "{}/app/installations/{install_id}/access_tokens",
                server.url()
            ))
            .header("Authorization", format!("Bearer {jwt}"))
            .json(&serde_json::json!({
                "repositories": ["not-authorized-repo"],
                "permissions": {"contents": "write"}
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 422);
        server.shutdown().await;
    }
}
