use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::auth::{
    BearerTokenError, InstallationTokenAccessError, authorize_installation_token,
    ensure_repo_permission,
};
use crate::server::SharedState;
use crate::state::{PermissionLevel, TokenPermission};

/// GET /repos/{owner}/{repo}/branches/{branch}
pub async fn get_branch(
    State(state): State<SharedState>,
    Path((owner, repo, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let state = state.read().await;
    let token = match authorize_installation_token(&headers, &state) {
        Ok(token) => token,
        Err(BearerTokenError::Missing) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"message": "Unauthorized"})),
            )
                .into_response();
        }
        Err(BearerTokenError::Invalid) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"message": "Bad credentials"})),
            )
                .into_response();
        }
    };

    if let Err(error) = ensure_repo_permission(
        &token,
        &repo,
        TokenPermission::Contents,
        PermissionLevel::Read,
    ) {
        return match error {
            InstallationTokenAccessError::RepoNotAccessible => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"message": "Not Found"})),
            )
                .into_response(),
            InstallationTokenAccessError::PermissionDenied => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"message": "Resource not accessible by integration"})),
            )
                .into_response(),
        };
    }

    // Find repository and check branch
    for repo_data in &state.repositories {
        if repo_data.owner == owner && repo_data.name == repo {
            if repo_data.branches.contains(&branch) {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "name": branch,
                        "commit": {
                            "sha": "abc123def456",
                        },
                        "protected": false,
                    })),
                )
                    .into_response();
            } else {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "message": "Branch not found"
                    })),
                )
                    .into_response();
            }
        }
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"message": "Not Found"})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use crate::server::TestServer;
    use crate::state::{AppOptions, AppState};
    use crate::test_support::{sign_test_jwt, test_http_client, test_rsa_private_key};

    async fn get_installation_token(
        client: &fabro_http::HttpClient,
        jwt: &str,
        owner: &str,
        repo: &str,
        base_url: &str,
    ) -> String {
        let resp = client
            .get(format!("{base_url}/repos/{owner}/{repo}/installation"))
            .header("Authorization", format!("Bearer {jwt}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let install_id = body["id"].as_u64().unwrap();

        let resp = client
            .post(format!(
                "{base_url}/app/installations/{install_id}/access_tokens"
            ))
            .header("Authorization", format!("Bearer {jwt}"))
            .json(&serde_json::json!({
                "repositories": [repo],
                "permissions": {"contents": "write"}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        body["token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn branch_exists_returns_200() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.to_string(),
            webhook_secret: None,
        });
        state.add_installation("100", "owner", vec!["repo".to_string()], false);
        state.add_repository(
            "owner",
            "repo",
            vec!["main".to_string(), "feature".to_string()],
            false,
        );
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("100", pem);
        let client = test_http_client();
        let token = get_installation_token(&client, &jwt, "owner", "repo", server.url()).await;

        let resp = client
            .get(format!(
                "{}/repos/owner/repo/branches/feature",
                server.url()
            ))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["name"], "feature");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn branch_not_found_returns_404() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.to_string(),
            webhook_secret: None,
        });
        state.add_installation("100", "owner", vec!["repo".to_string()], false);
        state.add_repository("owner", "repo", vec!["main".to_string()], false);
        let server = TestServer::start(state).await;

        let jwt = sign_test_jwt("100", pem);
        let client = test_http_client();
        let token = get_installation_token(&client, &jwt, "owner", "repo", server.url()).await;

        let resp = client
            .get(format!(
                "{}/repos/owner/repo/branches/nonexistent",
                server.url()
            ))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
        server.shutdown().await;
    }
}
