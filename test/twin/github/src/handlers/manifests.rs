use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::server::SharedState;

/// POST /app-manifests/{code}/conversions
pub async fn convert_manifest(
    State(state): State<SharedState>,
    Path(code): Path<String>,
) -> impl IntoResponse {
    let state = state.read().await;

    match state.manifest_conversions.get(&code) {
        Some(conversion) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "id": conversion.app_id,
                "slug": conversion.slug,
                "client_id": conversion.client_id,
                "client_secret": conversion.client_secret,
                "webhook_secret": conversion.webhook_secret,
                "pem": conversion.pem,
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
    use crate::state::{AppState, ManifestConversion};

    #[tokio::test]
    async fn manifest_conversion_returns_app_credentials() {
        let mut state = AppState::new();
        state.manifest_conversions.insert(
            "test-code".to_string(),
            ManifestConversion {
                code: "test-code".to_string(),
                app_id: 99,
                slug: "test-dev".to_string(),
                client_id: "Iv1.abc123".to_string(),
                client_secret: "secret123".to_string(),
                webhook_secret: Some("whsecret".to_string()),
                pem: "-----BEGIN RSA PRIVATE KEY-----\ntest\n-----END RSA PRIVATE KEY-----"
                    .to_string(),
            },
        );
        let server = TestServer::start(state).await;

        let client = crate::test_support::test_http_client();
        let resp = client
            .post(&format!(
                "{}/app-manifests/test-code/conversions",
                server.url()
            ))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["id"], 99);
        assert_eq!(body["slug"], "test-dev");
        assert_eq!(body["client_id"], "Iv1.abc123");
        assert!(body["pem"].as_str().is_some());

        server.shutdown().await;
    }

    #[tokio::test]
    async fn manifest_conversion_unknown_code_returns_404() {
        let state = AppState::new();
        let server = TestServer::start(state).await;

        let client = crate::test_support::test_http_client();
        let resp = client
            .post(&format!(
                "{}/app-manifests/unknown/conversions",
                server.url()
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
        server.shutdown().await;
    }
}
