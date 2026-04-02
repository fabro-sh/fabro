use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::auth::{
    BearerTokenError, InstallationTokenAccessError, authorize_installation_token,
    ensure_repo_permission,
};
use crate::server::SharedState;
use crate::state::{PermissionLevel, PullRequest, TokenPermission};

fn bearer_token_error_response(error: BearerTokenError) -> axum::response::Response {
    match error {
        BearerTokenError::Missing => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"message": "Unauthorized"})),
        )
            .into_response(),
        BearerTokenError::Invalid => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"message": "Bad credentials"})),
        )
            .into_response(),
    }
}

fn repo_permission_error_response(error: InstallationTokenAccessError) -> axum::response::Response {
    match error {
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
    }
}

/// POST /repos/{owner}/{repo}/pulls
pub async fn create_pull_request(
    State(state): State<SharedState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut state = state.write().await;
    let token = match authorize_installation_token(&headers, &state) {
        Ok(token) => token,
        Err(error) => return bearer_token_error_response(error),
    };
    if let Err(error) = ensure_repo_permission(
        &token,
        &repo,
        TokenPermission::PullRequests,
        PermissionLevel::Write,
    ) {
        return repo_permission_error_response(error);
    }

    let title = body
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let head = body
        .get("head")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let base = body
        .get("base")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let pr_body = body
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let draft = body.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    let number = state.next_pr_number;
    state.next_pr_number += 1;
    let node_id = format!("PR_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let html_url = format!("https://github.com/{owner}/{repo}/pull/{number}");

    let pr = PullRequest {
        number,
        node_id: node_id.clone(),
        title: title.clone(),
        body: pr_body.clone(),
        state: "open".to_string(),
        draft,
        mergeable: true,
        additions: 10,
        deletions: 5,
        changed_files: 2,
        html_url: html_url.clone(),
        user_login: "test-bot[bot]".to_string(),
        head_ref: head.clone(),
        base_ref: base.clone(),
        created_at: now.clone(),
        updated_at: now.clone(),
        auto_merge: None,
    };

    state
        .pull_requests
        .entry((owner.clone(), repo.clone()))
        .or_default()
        .push(pr);

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "number": number,
            "node_id": node_id,
            "title": title,
            "body": pr_body,
            "state": "open",
            "draft": draft,
            "html_url": html_url,
            "user": { "login": "test-bot[bot]" },
            "head": { "ref": head },
            "base": { "ref": base },
            "created_at": now,
            "updated_at": now,
        })),
    )
        .into_response()
}

/// GET /repos/{owner}/{repo}/pulls/{number}
pub async fn get_pull_request(
    State(state): State<SharedState>,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let state = state.read().await;
    let token = match authorize_installation_token(&headers, &state) {
        Ok(token) => token,
        Err(error) => return bearer_token_error_response(error),
    };
    if let Err(error) = ensure_repo_permission(
        &token,
        &repo,
        TokenPermission::PullRequests,
        PermissionLevel::Read,
    ) {
        return repo_permission_error_response(error);
    }

    let key = (owner, repo);
    if let Some(prs) = state.pull_requests.get(&key) {
        if let Some(pr) = prs.iter().find(|p| p.number == number) {
            return (StatusCode::OK, Json(pr_to_json(pr))).into_response();
        }
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"message": "Not Found"})),
    )
        .into_response()
}

/// PATCH /repos/{owner}/{repo}/pulls/{number}
pub async fn update_pull_request(
    State(state): State<SharedState>,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut state = state.write().await;
    let token = match authorize_installation_token(&headers, &state) {
        Ok(token) => token,
        Err(error) => return bearer_token_error_response(error),
    };
    if let Err(error) = ensure_repo_permission(
        &token,
        &repo,
        TokenPermission::PullRequests,
        PermissionLevel::Write,
    ) {
        return repo_permission_error_response(error);
    }

    let key = (owner, repo);
    if let Some(prs) = state.pull_requests.get_mut(&key) {
        if let Some(pr) = prs.iter_mut().find(|p| p.number == number) {
            if let Some(new_state) = body.get("state").and_then(|v| v.as_str()) {
                pr.state = new_state.to_string();
            }
            if let Some(new_title) = body.get("title").and_then(|v| v.as_str()) {
                pr.title = new_title.to_string();
            }
            if let Some(new_body) = body.get("body").and_then(|v| v.as_str()) {
                pr.body = new_body.to_string();
            }
            pr.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            return (StatusCode::OK, Json(pr_to_json(pr))).into_response();
        }
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"message": "Not Found"})),
    )
        .into_response()
}

/// PUT /repos/{owner}/{repo}/pulls/{number}/merge
pub async fn merge_pull_request(
    State(state): State<SharedState>,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    headers: HeaderMap,
    Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut state = state.write().await;
    let token = match authorize_installation_token(&headers, &state) {
        Ok(token) => token,
        Err(error) => return bearer_token_error_response(error),
    };
    if let Err(error) = ensure_repo_permission(
        &token,
        &repo,
        TokenPermission::PullRequests,
        PermissionLevel::Write,
    ) {
        return repo_permission_error_response(error);
    }

    let key = (owner, repo);
    if let Some(prs) = state.pull_requests.get_mut(&key) {
        if let Some(pr) = prs.iter_mut().find(|p| p.number == number) {
            if pr.state != "open" || !pr.mergeable {
                return (
                    StatusCode::METHOD_NOT_ALLOWED,
                    Json(serde_json::json!({
                        "message": "Pull request is not mergeable"
                    })),
                )
                    .into_response();
            }
            pr.state = "closed".to_string();
            pr.mergeable = false;
            pr.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "sha": "abc123",
                    "merged": true,
                    "message": "Pull Request successfully merged"
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

fn pr_to_json(pr: &PullRequest) -> serde_json::Value {
    let mut json = serde_json::json!({
        "number": pr.number,
        "node_id": pr.node_id,
        "title": pr.title,
        "body": pr.body,
        "state": pr.state,
        "draft": pr.draft,
        "mergeable": pr.mergeable,
        "additions": pr.additions,
        "deletions": pr.deletions,
        "changed_files": pr.changed_files,
        "html_url": pr.html_url,
        "user": { "login": pr.user_login },
        "head": { "ref": pr.head_ref },
        "base": { "ref": pr.base_ref },
        "created_at": pr.created_at,
        "updated_at": pr.updated_at,
    });

    if let Some(auto_merge) = &pr.auto_merge {
        json["auto_merge"] = serde_json::json!({
            "enabled_at": auto_merge.enabled_at,
            "merge_method": auto_merge.merge_method,
        });
    }

    json
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

    async fn get_installation_token(
        client: &reqwest::Client,
        jwt: &str,
        owner: &str,
        repo: &str,
        base_url: &str,
    ) -> String {
        let resp = client
            .get(&format!("{base_url}/repos/{owner}/{repo}/installation"))
            .header("Authorization", format!("Bearer {jwt}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let install_id = body["id"].as_u64().unwrap();

        let resp = client
            .post(&format!(
                "{base_url}/app/installations/{install_id}/access_tokens"
            ))
            .header("Authorization", format!("Bearer {jwt}"))
            .json(&serde_json::json!({
                "repositories": [repo],
                "permissions": {"contents": "write", "pull_requests": "write"}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        body["token"].as_str().unwrap().to_string()
    }

    async fn setup_and_get_token(
        state: &mut AppState,
        pem: &str,
    ) -> (TestServer, reqwest::Client, String) {
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
        let server = TestServer::start(state.clone()).await;

        let jwt = sign_test_jwt("100", pem);
        let client = reqwest::Client::new();
        let token = get_installation_token(&client, &jwt, "owner", "repo", server.url()).await;

        (server, client, token)
    }

    #[tokio::test]
    async fn create_pr_returns_201() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        let (server, client, token) = setup_and_get_token(&mut state, &pem).await;

        let resp = client
            .post(&format!("{}/repos/owner/repo/pulls", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .json(&serde_json::json!({
                "title": "Test PR",
                "head": "feature",
                "base": "main",
                "body": "PR body",
                "draft": false,
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["title"], "Test PR");
        assert!(body["number"].as_u64().is_some());
        assert!(body["html_url"].as_str().is_some());
        assert!(body["node_id"].as_str().is_some());
        server.shutdown().await;
    }

    #[tokio::test]
    async fn get_pr_returns_detail() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        let (server, client, token) = setup_and_get_token(&mut state, &pem).await;

        // Create a PR first
        let create_resp = client
            .post(&format!("{}/repos/owner/repo/pulls", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "title": "Test PR",
                "head": "feature",
                "base": "main",
                "body": "Body text",
                "draft": true,
            }))
            .send()
            .await
            .unwrap();
        let created: serde_json::Value = create_resp.json().await.unwrap();
        let number = created["number"].as_u64().unwrap();

        // Now get it
        let resp = client
            .get(&format!("{}/repos/owner/repo/pulls/{number}", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["number"], number);
        assert_eq!(body["title"], "Test PR");
        assert_eq!(body["state"], "open");
        assert_eq!(body["draft"], true);
        assert!(body["mergeable"].is_boolean());
        assert!(body["additions"].is_u64());
        assert!(body["deletions"].is_u64());
        assert!(body["changed_files"].is_u64());
        assert_eq!(body["user"]["login"], "test-bot[bot]");
        assert_eq!(body["head"]["ref"], "feature");
        assert_eq!(body["base"]["ref"], "main");
        assert!(body["created_at"].is_string());
        assert!(body["updated_at"].is_string());

        server.shutdown().await;
    }

    #[tokio::test]
    async fn merge_pr_returns_200() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        let (server, client, token) = setup_and_get_token(&mut state, &pem).await;

        // Create a PR
        let create_resp = client
            .post(&format!("{}/repos/owner/repo/pulls", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "title": "Test PR", "head": "feature", "base": "main", "body": "", "draft": false,
            }))
            .send()
            .await
            .unwrap();
        let created: serde_json::Value = create_resp.json().await.unwrap();
        let number = created["number"].as_u64().unwrap();

        // Merge it
        let merge_resp = client
            .put(&format!(
                "{}/repos/owner/repo/pulls/{number}/merge",
                server.url()
            ))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({ "merge_method": "squash" }))
            .send()
            .await
            .unwrap();
        assert_eq!(merge_resp.status(), 200);

        // Verify state changed
        let get_resp = client
            .get(&format!("{}/repos/owner/repo/pulls/{number}", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = get_resp.json().await.unwrap();
        assert_eq!(body["state"], "closed");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn close_pr_returns_200() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        let (server, client, token) = setup_and_get_token(&mut state, &pem).await;

        // Create a PR
        let create_resp = client
            .post(&format!("{}/repos/owner/repo/pulls", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "title": "Test PR", "head": "feature", "base": "main", "body": "", "draft": false,
            }))
            .send()
            .await
            .unwrap();
        let created: serde_json::Value = create_resp.json().await.unwrap();
        let number = created["number"].as_u64().unwrap();

        // Close it
        let close_resp = client
            .patch(&format!("{}/repos/owner/repo/pulls/{number}", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({ "state": "closed" }))
            .send()
            .await
            .unwrap();
        assert_eq!(close_resp.status(), 200);

        server.shutdown().await;
    }

    #[tokio::test]
    async fn merge_nonexistent_pr_returns_404() {
        let pem = test_rsa_key();
        let mut state = AppState::new();
        let (server, client, token) = setup_and_get_token(&mut state, &pem).await;

        let resp = client
            .put(&format!(
                "{}/repos/owner/repo/pulls/999/merge",
                server.url()
            ))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({ "merge_method": "squash" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        server.shutdown().await;
    }
}
