use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::server::SharedState;

/// GET /repos/{owner}/{repo}/releases/latest
pub async fn get_latest_release(
    State(state): State<SharedState>,
    Path((owner, repo)): Path<(String, String)>,
) -> impl IntoResponse {
    let state = state.read().await;

    match state.releases.get(&(owner, repo)) {
        Some(release) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "tag_name": release.tag_name,
            })),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"message": "Not Found"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::server::TestServer;
    use crate::state::{AppState, Release};

    #[tokio::test]
    async fn latest_release_returns_tag() {
        let mut state = AppState::new();
        state.releases.insert(
            ("test-org".to_string(), "test-project".to_string()),
            Release {
                tag_name: "v0.176.2".to_string(),
            },
        );
        let server = TestServer::start(state).await;

        let resp = crate::test_support::test_http_client()
            .get(format!(
                "{}/repos/test-org/test-project/releases/latest",
                server.url()
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["tag_name"], "v0.176.2");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn latest_release_404_when_none() {
        let state = AppState::new();
        let server = TestServer::start(state).await;

        let resp = crate::test_support::test_http_client()
            .get(format!("{}/repos/owner/repo/releases/latest", server.url()))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
        server.shutdown().await;
    }
}
