use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::Storage;
use fabro_model::Provider;
use fabro_server::install::{InstallAppState, build_install_router};
use fabro_vault::Vault;
use httpmock::MockServer;
use tower::ServiceExt;

use crate::helpers::body_json;

#[tokio::test]
async fn install_router_isolated_from_normal_api_surface() {
    let app = build_install_router(InstallAppState::for_test("test-install-token"));

    let health_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health_response.status(), StatusCode::OK);
    let health_body = body_json(health_response.into_body()).await;
    assert_eq!(health_body["status"], "ok");
    assert_eq!(health_body["mode"], "install");

    let root_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header("accept", "text/html,application/xhtml+xml")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(root_response.status(), StatusCode::OK);
    let root_html = String::from_utf8(
        axum::body::to_bytes(root_response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        root_html.contains("__FABRO_MODE__ = \"install\""),
        "install shell should mark the SPA boot mode"
    );

    let api_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(api_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn install_session_requires_valid_install_token() {
    let app = build_install_router(InstallAppState::for_test("test-install-token"));

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/session")
                .header("authorization", "Bearer test-install-token")
                .header("x-forwarded-proto", "https")
                .header("x-forwarded-host", "fabro.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let body = body_json(authorized.into_body()).await;
    assert_eq!(
        body["prefill"]["canonical_url"],
        "https://fabro.example.com"
    );
}

#[tokio::test]
async fn install_endpoints_reject_missing_and_wrong_tokens() {
    let app = build_install_router(InstallAppState::for_test("test-install-token"));
    let cases = [
        ("GET", "/install/session", None),
        (
            "POST",
            "/install/llm/test",
            Some(r#"{"provider":"anthropic","api_key":"anthropic-test-key"}"#),
        ),
        (
            "PUT",
            "/install/llm",
            Some(r#"{"providers":[{"provider":"anthropic","api_key":"anthropic-test-key"}]}"#),
        ),
        (
            "PUT",
            "/install/server",
            Some(r#"{"canonical_url":"https://fabro.example.com"}"#),
        ),
        (
            "POST",
            "/install/github/token/test",
            Some(r#"{"token":"ghp_test_token"}"#),
        ),
        (
            "PUT",
            "/install/github/token",
            Some(r#"{"token":"ghp_test_token","username":"octocat"}"#),
        ),
        (
            "POST",
            "/install/github/app/manifest",
            Some(r#"{"owner":"personal","app_name":"Fabro","allowed_username":"octocat"}"#),
        ),
        ("POST", "/install/finish", None),
    ];

    for (method, path, body) in cases {
        let mut missing_token = Request::builder().method(method).uri(path);
        let mut wrong_token = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", "Bearer wrong-token");
        if body.is_some() {
            missing_token = missing_token.header("content-type", "application/json");
            wrong_token = wrong_token.header("content-type", "application/json");
        }

        let missing_token_response = app
            .clone()
            .oneshot(
                missing_token
                    .body(Body::from(body.unwrap_or_default().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            missing_token_response.status(),
            StatusCode::UNAUTHORIZED,
            "missing token should be rejected for {method} {path}"
        );

        let wrong_token_response = app
            .clone()
            .oneshot(
                wrong_token
                    .body(Body::from(body.unwrap_or_default().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            wrong_token_response.status(),
            StatusCode::UNAUTHORIZED,
            "wrong token should be rejected for {method} {path}"
        );
    }
}

#[tokio::test]
async fn token_install_finish_persists_settings_env_and_vault() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    let app = build_install_router(InstallAppState::for_test_with_paths(
        "test-install-token",
        temp_dir.path(),
        &config_path,
    ));

    let llm_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/llm")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"providers":[{"provider":"anthropic","api_key":"anthropic-test-key"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(llm_response.status(), StatusCode::NO_CONTENT);

    let server_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/server")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"canonical_url":"https://fabro.example.com"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(server_response.status(), StatusCode::NO_CONTENT);

    let github_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/github/token")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"token":"ghp_test_token","username":"brynary"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(github_response.status(), StatusCode::NO_CONTENT);

    let finish_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/finish")
                .header("authorization", "Bearer test-install-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(finish_response.status(), StatusCode::ACCEPTED);
    let finish_body = body_json(finish_response.into_body()).await;
    assert_eq!(finish_body["status"], "completing");
    assert_eq!(finish_body["restart_url"], "https://fabro.example.com");
    assert!(
        finish_body["dev_token"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );

    let settings = std::fs::read_to_string(&config_path).unwrap();
    assert!(settings.contains("https://fabro.example.com"));
    assert!(settings.contains("strategy = \"token\""));

    let server_env = std::fs::read_to_string(
        fabro_config::Storage::new(temp_dir.path())
            .server_state()
            .env_path(),
    )
    .unwrap();
    assert!(server_env.contains("FABRO_JWT_PRIVATE_KEY="));
    assert!(server_env.contains("FABRO_JWT_PUBLIC_KEY="));
    assert!(server_env.contains("SESSION_SECRET="));
    assert!(server_env.contains("FABRO_DEV_TOKEN="));

    let vault = Vault::load(fabro_config::Storage::new(temp_dir.path()).secrets_path()).unwrap();
    assert!(vault.get("anthropic").is_some());
    assert_eq!(vault.get("GITHUB_TOKEN"), Some("ghp_test_token"));
}

#[tokio::test]
async fn install_validation_endpoints_validate_credentials_and_github_token() {
    let llm_mock = MockServer::start_async().await;
    llm_mock
        .mock_async(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    serde_json::to_string(&serde_json::json!({
                        "id": "msg_test_123",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-5",
                        "content": [{"type": "text", "text": "OK"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 10, "output_tokens": 5}
                    }))
                    .unwrap(),
                );
        })
        .await;
    let github_mock = MockServer::start_async().await;
    github_mock
        .mock_async(|when, then| {
            when.method("GET").path("/user");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"login":"octocat"}"#);
        })
        .await;

    let app = build_install_router(
        InstallAppState::for_test("test-install-token")
            .with_provider_base_url(Provider::Anthropic, format!("{}/v1", llm_mock.url("")))
            .with_github_api_base_url(github_mock.url("")),
    );

    let llm_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/llm/test")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"provider":"anthropic","api_key":"anthropic-test-key"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(llm_response.status(), StatusCode::OK);
    let llm_body = body_json(llm_response.into_body()).await;
    assert_eq!(llm_body["ok"], true);

    let github_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/token/test")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"token":"ghp_test_token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(github_response.status(), StatusCode::OK);
    let github_body = body_json(github_response.into_body()).await;
    assert_eq!(github_body["username"], "octocat");
}

#[tokio::test]
async fn github_app_manifest_round_trip_updates_install_session() {
    let github_mock = MockServer::start_async().await;
    let conversion_mock = github_mock
        .mock_async(|when, then| {
            when.method("POST")
                .path("/app-manifests/stub-code/conversions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": 42,
                        "slug": "fabro-test-app",
                        "client_id": "Iv1.test-client-id",
                        "client_secret": "test-client-secret",
                        "webhook_secret": "test-webhook-secret",
                        "pem": "-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----\n"
                    }"#,
                );
        })
        .await;
    let app = build_install_router(
        InstallAppState::for_test("test-install-token")
            .with_github_api_base_url(github_mock.url("")),
    );

    let server_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/server")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"canonical_url":"https://fabro.example.com"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(server_response.status(), StatusCode::NO_CONTENT);

    let manifest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":"personal","app_name":"Fabro Test","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest_response.status(), StatusCode::OK);
    let manifest_body = body_json(manifest_response.into_body()).await;
    assert_eq!(
        manifest_body["github_form_action"],
        "https://github.com/settings/apps/new"
    );
    assert_eq!(
        manifest_body["manifest"]["callback_urls"][0],
        "https://fabro.example.com/auth/callback/github"
    );

    let redirect_url = manifest_body["manifest"]["redirect_url"]
        .as_str()
        .expect("redirect_url should be present");
    let redirect_uri = fabro_http::Url::parse(redirect_url).unwrap();
    let state = redirect_uri
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("state should be embedded in redirect_url");

    let callback_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/install/github/app/redirect?code=stub-code&state={state}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(callback_response.status(), StatusCode::FOUND);
    assert_eq!(
        callback_response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/install/github/done?token=test-install-token")
    );
    conversion_mock.assert_async().await;

    let session_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/session")
                .header("authorization", "Bearer test-install-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_response.status(), StatusCode::OK);
    let session_body = body_json(session_response.into_body()).await;
    assert_eq!(session_body["github"]["strategy"], "app");
    assert_eq!(session_body["github"]["slug"], "fabro-test-app");
    assert_eq!(session_body["github"]["allowed_username"], "octocat");
    assert!(
        session_body["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "github")
    );
}

#[tokio::test]
async fn github_app_redirect_rejects_invalid_or_missing_state_without_mutating_session() {
    let github_mock = MockServer::start_async().await;
    let conversion_mock = github_mock
        .mock_async(|when, then| {
            when.method("POST")
                .path("/app-manifests/stub-code/conversions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": 42,
                        "slug": "fabro-test-app",
                        "client_id": "Iv1.test-client-id",
                        "client_secret": "test-client-secret",
                        "webhook_secret": "test-webhook-secret",
                        "pem": "-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----\n"
                    }"#,
                );
        })
        .await;
    let app = build_install_router(
        InstallAppState::for_test("test-install-token")
            .with_github_api_base_url(github_mock.url("")),
    );

    let server_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/server")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"canonical_url":"https://fabro.example.com"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(server_response.status(), StatusCode::NO_CONTENT);

    let manifest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":"personal","app_name":"Fabro Test","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest_response.status(), StatusCode::OK);
    let manifest_body = body_json(manifest_response.into_body()).await;
    let redirect_url = manifest_body["manifest"]["redirect_url"]
        .as_str()
        .expect("redirect_url should be present");
    let redirect_uri = fabro_http::Url::parse(redirect_url).unwrap();
    let state = redirect_uri
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("state should be embedded in redirect_url");

    let wrong_state_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/github/app/redirect?code=stub-code&state=wrong-state")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_state_response.status(), StatusCode::BAD_REQUEST);
    conversion_mock.assert_calls_async(0).await;

    let session_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/session")
                .header("authorization", "Bearer test-install-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_response.status(), StatusCode::OK);
    let session_body = body_json(session_response.into_body()).await;
    assert!(session_body["github"].is_null());
    assert!(
        !session_body["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "github")
    );

    let missing_state_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/github/app/redirect?code=stub-code")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_state_response.status(), StatusCode::BAD_REQUEST);
    conversion_mock.assert_calls_async(0).await;

    let valid_state_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/install/github/app/redirect?code=stub-code&state={state}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(valid_state_response.status(), StatusCode::FOUND);
    conversion_mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn install_finish_failure_restores_settings_and_vault_but_leaves_env_keys() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    std::fs::write(&config_path, "_version = 1\n[project]\nname = \"keep\"\n").unwrap();

    let storage = Storage::new(temp_dir.path());
    let vault_path = storage.secrets_path();
    std::fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    std::fs::write(&vault_path, "{ not valid json").unwrap();

    let app = build_install_router(InstallAppState::for_test_with_paths(
        "test-install-token",
        temp_dir.path(),
        &config_path,
    ));

    let llm_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/llm")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"providers":[{"provider":"anthropic","api_key":"anthropic-test-key"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(llm_response.status(), StatusCode::NO_CONTENT);

    let server_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/server")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"canonical_url":"https://fabro.example.com"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(server_response.status(), StatusCode::NO_CONTENT);

    let github_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/github/token")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"token":"ghp_test_token","username":"brynary"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(github_response.status(), StatusCode::NO_CONTENT);

    let finish_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/finish")
                .header("authorization", "Bearer test-install-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(finish_response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let finish_body = body_json(finish_response.into_body()).await;
    assert!(
        finish_body["error"]
            .as_str()
            .is_some_and(|value| value.contains("persisting install outputs directly"))
    );
    let leftover_env_keys = finish_body["leftover_env_keys"]
        .as_array()
        .expect("leftover_env_keys should be present");
    assert!(
        leftover_env_keys
            .iter()
            .any(|value| value == "SESSION_SECRET")
    );
    assert!(
        leftover_env_keys
            .iter()
            .any(|value| value == "FABRO_DEV_TOKEN")
    );

    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "_version = 1\n[project]\nname = \"keep\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(&vault_path).unwrap(),
        "{ not valid json"
    );

    let server_env = std::fs::read_to_string(storage.server_state().env_path()).unwrap();
    assert!(server_env.contains("SESSION_SECRET="));
    assert!(server_env.contains("FABRO_DEV_TOKEN="));

    let session_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/session")
                .header("authorization", "Bearer test-install-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_response.status(), StatusCode::OK);
    let session_body = body_json(session_response.into_body()).await;
    assert!(
        session_body["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "github")
    );
}
