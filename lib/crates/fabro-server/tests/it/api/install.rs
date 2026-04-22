#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::{Storage, parse_settings_layer, resolve_server_from_file};
use fabro_install::OBJECT_STORE_MANAGED_COMMENT;
use fabro_model::Provider;
use fabro_server::install::{InstallAppState, build_install_router};
use fabro_util::{Home, dev_token};
use fabro_vault::Vault;
use httpmock::MockServer;
use tokio::time::sleep;
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use crate::helpers::{checked_response, response_json, response_status, response_text};

#[derive(Default)]
struct EventCapture {
    fields: Vec<(String, String)>,
}

impl Visit for EventCapture {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .push((field.name().to_string(), format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
}

struct CaptureLayer {
    lines: Arc<StdMutex<Vec<String>>>,
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !event
            .metadata()
            .target()
            .starts_with("fabro_server::install")
        {
            return;
        }

        let mut capture = EventCapture::default();
        event.record(&mut capture);

        let mut line = event.metadata().level().to_string();
        for (field, value) in capture.fields {
            let _ = write!(line, " {field}={value}");
        }
        self.lines.lock().unwrap().push(line);
    }
}

async fn put_install_server(app: &axum::Router, token: &str, canonical_url: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/server")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"canonical_url":"{canonical_url}"}}"#
                )))
                .expect("server install request should build"),
        )
        .await
        .unwrap();
    response_status(response, StatusCode::NO_CONTENT, "PUT /install/server").await;
}

async fn put_install_llm(app: &axum::Router, token: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/llm")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"providers":[{"provider":"anthropic","api_key":"anthropic-test-key"}]}"#,
                ))
                .expect("LLM install request should build"),
        )
        .await
        .unwrap();
    response_status(response, StatusCode::NO_CONTENT, "PUT /install/llm").await;
}

async fn put_install_github_token(app: &axum::Router, token: &str, username: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/github/token")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"token":"ghp_test_token","username":"{username}"}}"#
                )))
                .expect("GitHub token install request should build"),
        )
        .await
        .unwrap();
    response_status(
        response,
        StatusCode::NO_CONTENT,
        "PUT /install/github/token",
    )
    .await;
}

async fn put_install_object_store(app: &axum::Router, token: &str, body: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/object-store")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("object-store install request should build"),
        )
        .await
        .unwrap();
    response_status(
        response,
        StatusCode::NO_CONTENT,
        "PUT /install/object-store",
    )
    .await;
}

async fn put_install_object_store_local(app: &axum::Router, token: &str) {
    put_install_object_store(app, token, r#"{"provider":"local"}"#).await;
}

async fn configure_token_install(app: &axum::Router, token: &str) {
    put_install_server(app, token, "https://fabro.example.com").await;
    put_install_object_store_local(app, token).await;
    put_install_llm(app, token).await;
    put_install_github_token(app, token, "brynary").await;
}

#[tokio::test]
async fn install_router_isolated_from_normal_api_surface() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;

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
    let health_body = response_json(health_response, StatusCode::OK, "GET /health").await;
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
    let root_html = response_text(root_response, StatusCode::OK, "GET /").await;
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
    response_status(api_response, StatusCode::NOT_FOUND, "GET /api/v1/auth/me").await;
}

#[tokio::test]
async fn install_session_requires_valid_install_token() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;

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
    response_status(
        unauthorized,
        StatusCode::UNAUTHORIZED,
        "GET /install/session",
    )
    .await;

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
    let body = response_json(authorized, StatusCode::OK, "GET /install/session").await;
    assert_eq!(
        body["prefill"]["canonical_url"],
        "https://fabro.example.com"
    );
}

#[tokio::test]
async fn install_endpoints_reject_missing_and_wrong_tokens() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;
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
            "/install/object-store/test",
            Some(r#"{"provider":"local"}"#),
        ),
        (
            "PUT",
            "/install/object-store",
            Some(r#"{"provider":"local"}"#),
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
            Some(
                r#"{"owner":{"kind":"personal"},"app_name":"Fabro","allowed_username":"octocat"}"#,
            ),
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
        response_status(
            missing_token_response,
            StatusCode::UNAUTHORIZED,
            format!("{method} {path} without install token"),
        )
        .await;

        let wrong_token_response = app
            .clone()
            .oneshot(
                wrong_token
                    .body(Body::from(body.unwrap_or_default().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        response_status(
            wrong_token_response,
            StatusCode::UNAUTHORIZED,
            format!("{method} {path} with wrong install token"),
        )
        .await;
    }
}

#[tokio::test]
async fn install_endpoints_accept_query_token_when_authorization_header_is_wrong() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install/session?token=test-install-token")
                .header("authorization", "Bearer wrong-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    response_status(response, StatusCode::OK, "GET /install/session?token=...").await;
}

#[tokio::test]
async fn object_store_local_validation_and_save_update_install_session() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;

    let validation_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/object-store/test")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"provider":"local"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let validation_body = response_json(
        validation_response,
        StatusCode::OK,
        "POST /install/object-store/test",
    )
    .await;
    assert_eq!(validation_body["ok"], true);

    put_install_object_store_local(&app, "test-install-token").await;

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
    let session_body =
        response_json(session_response, StatusCode::OK, "GET /install/session").await;
    assert_eq!(session_body["object_store"]["provider"], "local");
    assert!(
        session_body["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "object_store")
    );
}

#[tokio::test]
async fn object_store_validation_rejects_runtime_mode_access_keys_without_echoing_secrets() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;
    let access_key_id = "AKIA_RUNTIME_SHOULD_NOT_LEAK";
    let secret_access_key = "runtime-secret-should-not-leak";

    let validation_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/object-store/test")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"runtime","access_key_id":"{access_key_id}","secret_access_key":"{secret_access_key}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    let validation_body = response_json(
        validation_response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /install/object-store/test",
    )
    .await;

    assert_eq!(
        validation_body["errors"][0]["detail"],
        "AWS access key fields are only allowed when using manual AWS access key credentials."
    );
    let rendered = validation_body.to_string();
    assert!(!rendered.contains(access_key_id));
    assert!(!rendered.contains(secret_access_key));
}

#[tokio::test]
async fn install_finish_requires_object_store_step() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;

    put_install_server(&app, "test-install-token", "https://fabro.example.com").await;
    put_install_llm(&app, "test-install-token").await;
    put_install_github_token(&app, "test-install-token", "brynary").await;

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
    let finish_body = response_json(
        finish_response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /install/finish",
    )
    .await;
    assert_eq!(
        finish_body["errors"][0]["detail"],
        "install step 'object_store' is incomplete"
    );
}

#[tokio::test]
async fn manual_object_store_session_is_redacted_and_blank_resubmit_preserves_credentials() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    let app = build_install_router(InstallAppState::for_test_with_paths(
        "test-install-token",
        temp_dir.path(),
        &config_path,
    ))
    .await;

    put_install_object_store(
        &app,
        "test-install-token",
        r#"{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"access_key","access_key_id":"AKIA_TEST_VALUE","secret_access_key":"secret-test-value"}"#,
    )
    .await;

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
    let session_body =
        response_json(session_response, StatusCode::OK, "GET /install/session").await;
    assert_eq!(session_body["object_store"]["provider"], "s3");
    assert_eq!(session_body["object_store"]["bucket"], "fabro-data");
    assert_eq!(session_body["object_store"]["region"], "us-east-1");
    assert_eq!(
        session_body["object_store"]["credential_mode"],
        "access_key"
    );
    assert_eq!(
        session_body["object_store"]["manual_credentials_saved"],
        true
    );
    let rendered_session = session_body.to_string();
    assert!(!rendered_session.contains("AKIA_TEST_VALUE"));
    assert!(!rendered_session.contains("secret-test-value"));

    put_install_object_store(
        &app,
        "test-install-token",
        r#"{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"access_key"}"#,
    )
    .await;
    put_install_server(&app, "test-install-token", "https://fabro.example.com").await;
    put_install_llm(&app, "test-install-token").await;
    put_install_github_token(&app, "test-install-token", "brynary").await;

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
    response_status(
        finish_response,
        StatusCode::ACCEPTED,
        "POST /install/finish",
    )
    .await;

    let server_env =
        std::fs::read_to_string(Storage::new(temp_dir.path()).runtime_directory().env_path())
            .unwrap();
    assert!(server_env.contains("AWS_ACCESS_KEY_ID=AKIA_TEST_VALUE"));
    assert!(server_env.contains("AWS_SECRET_ACCESS_KEY=secret-test-value"));
}

#[tokio::test]
async fn switching_object_store_from_manual_to_runtime_clears_saved_manual_credentials() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    let app = build_install_router(InstallAppState::for_test_with_paths(
        "test-install-token",
        temp_dir.path(),
        &config_path,
    ))
    .await;

    put_install_object_store(
        &app,
        "test-install-token",
        r#"{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"access_key","access_key_id":"AKIA_SWITCH_ME","secret_access_key":"switch-secret-value"}"#,
    )
    .await;
    put_install_object_store(
        &app,
        "test-install-token",
        r#"{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"runtime"}"#,
    )
    .await;

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
    let session_body =
        response_json(session_response, StatusCode::OK, "GET /install/session").await;
    assert_eq!(session_body["object_store"]["provider"], "s3");
    assert_eq!(session_body["object_store"]["credential_mode"], "runtime");
    assert_eq!(
        session_body["object_store"]["manual_credentials_saved"],
        false
    );
    let rendered_session = session_body.to_string();
    assert!(!rendered_session.contains("AKIA_SWITCH_ME"));
    assert!(!rendered_session.contains("switch-secret-value"));

    put_install_server(&app, "test-install-token", "https://fabro.example.com").await;
    put_install_llm(&app, "test-install-token").await;
    put_install_github_token(&app, "test-install-token", "brynary").await;

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
    response_status(
        finish_response,
        StatusCode::ACCEPTED,
        "POST /install/finish",
    )
    .await;

    let server_env =
        std::fs::read_to_string(Storage::new(temp_dir.path()).runtime_directory().env_path())
            .unwrap();
    assert!(!server_env.contains("AWS_ACCESS_KEY_ID="));
    assert!(!server_env.contains("AWS_SECRET_ACCESS_KEY="));
}

#[tokio::test]
async fn runtime_object_store_finish_removes_managed_aws_keys_but_keeps_unmarked_entries() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    let storage = Storage::new(temp_dir.path());
    std::fs::write(
        storage.runtime_directory().env_path(),
        format!(
            "AWS_ACCESS_KEY_ID=operator-id\n# {OBJECT_STORE_MANAGED_COMMENT}\nAWS_SECRET_ACCESS_KEY=managed-secret\nKEEP_ME=1\n"
        ),
    )
    .unwrap();

    let app = build_install_router(InstallAppState::for_test_with_paths(
        "test-install-token",
        temp_dir.path(),
        &config_path,
    ))
    .await;

    put_install_server(&app, "test-install-token", "https://fabro.example.com").await;
    put_install_object_store(
        &app,
        "test-install-token",
        r#"{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"runtime"}"#,
    )
    .await;
    put_install_llm(&app, "test-install-token").await;
    put_install_github_token(&app, "test-install-token", "brynary").await;

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
    response_status(
        finish_response,
        StatusCode::ACCEPTED,
        "POST /install/finish",
    )
    .await;

    let server_env = std::fs::read_to_string(storage.runtime_directory().env_path()).unwrap();
    assert!(server_env.contains("AWS_ACCESS_KEY_ID=operator-id"));
    assert!(!server_env.contains("managed-secret"));
    assert!(server_env.contains("KEEP_ME=1"));
}

#[tokio::test]
async fn token_install_finish_persists_settings_env_and_vault() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    let app = build_install_router(InstallAppState::for_test_with_paths(
        "test-install-token",
        temp_dir.path(),
        &config_path,
    ))
    .await;
    configure_token_install(&app, "test-install-token").await;

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
    let finish_body = response_json(
        finish_response,
        StatusCode::ACCEPTED,
        "POST /install/finish",
    )
    .await;
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
    let parsed = parse_settings_layer(&settings).expect("settings should parse");
    let resolved = resolve_server_from_file(&parsed).expect("settings should resolve");
    assert_eq!(
        match resolved.listen {
            fabro_types::settings::server::ServerListenSettings::Tcp { address, .. } => {
                address.to_string()
            }
            fabro_types::settings::server::ServerListenSettings::Unix { .. } => {
                String::new()
            }
        },
        "127.0.0.1:32276"
    );

    let server_env = std::fs::read_to_string(
        fabro_config::Storage::new(temp_dir.path())
            .runtime_directory()
            .env_path(),
    )
    .unwrap();
    assert!(server_env.contains("FABRO_JWT_PRIVATE_KEY="));
    assert!(server_env.contains("FABRO_JWT_PUBLIC_KEY="));
    assert!(server_env.contains("SESSION_SECRET="));
    assert!(server_env.contains("FABRO_DEV_TOKEN="));
    assert!(!server_env.contains("AWS_ACCESS_KEY_ID="));
    assert!(!server_env.contains("AWS_SECRET_ACCESS_KEY="));

    let vault = Vault::load(fabro_config::Storage::new(temp_dir.path()).secrets_path()).unwrap();
    assert!(vault.get("anthropic").is_some());
    assert_eq!(vault.get("GITHUB_TOKEN"), Some("ghp_test_token"));
}

#[tokio::test]
async fn app_install_finish_omits_dev_token_and_does_not_write_it() {
    let temp_dir = tempfile::tempdir().unwrap();
    let home_root = tempfile::tempdir().unwrap();
    let home = Home::new(home_root.path().join(".fabro"));
    let config_path = temp_dir.path().join("settings.toml");
    let github_mock = MockServer::start_async().await;
    github_mock
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
        InstallAppState::for_test_with_paths("test-install-token", temp_dir.path(), &config_path)
            .with_home(home.clone())
            .with_github_api_base_url(github_mock.url("")),
    )
    .await;

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
    response_status(llm_response, StatusCode::NO_CONTENT, "PUT /install/llm").await;

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
    response_status(
        server_response,
        StatusCode::NO_CONTENT,
        "PUT /install/server",
    )
    .await;

    put_install_object_store_local(&app, "test-install-token").await;

    let manifest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":{"kind":"personal"},"app_name":"Fabro Test","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let manifest_body = response_json(
        manifest_response,
        StatusCode::OK,
        "POST /install/github/app/manifest",
    )
    .await;
    let state = manifest_body["state"]
        .as_str()
        .expect("state should be present on manifest response")
        .to_owned();
    assert_eq!(
        manifest_body["manifest"]["redirect_url"],
        "https://fabro.example.com/install/github/app/redirect",
        "redirect_url must not carry a query string — GitHub rejects manifests whose redirect_url has one"
    );

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
    response_status(
        callback_response,
        StatusCode::FOUND,
        "GET /install/github/app/redirect?code=stub-code&state=...",
    )
    .await;

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
    let finish_body = response_json(
        finish_response,
        StatusCode::ACCEPTED,
        "POST /install/finish",
    )
    .await;
    assert_eq!(finish_body["status"], "completing");
    assert_eq!(finish_body["restart_url"], "https://fabro.example.com");
    assert!(
        finish_body.get("dev_token").is_none(),
        "App installs must not expose a dev token"
    );

    let server_env =
        std::fs::read_to_string(Storage::new(temp_dir.path()).runtime_directory().env_path())
            .unwrap();
    assert!(!server_env.contains("FABRO_DEV_TOKEN="));

    assert!(
        !home.dev_token_path().exists(),
        "home dev token file should not be created for App installs"
    );
    assert!(
        !Storage::new(temp_dir.path())
            .runtime_directory()
            .dev_token_path()
            .exists(),
        "storage dev token file should not be created for App installs"
    );
}

#[tokio::test]
async fn token_install_finish_invokes_shutdown_callback_after_accepting() {
    let temp_dir = tempfile::tempdir().unwrap();
    let home_root = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    let callback_invoked = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_invoked);
    let app = build_install_router(
        InstallAppState::for_test_with_paths("test-install-token", temp_dir.path(), &config_path)
            .with_home(Home::new(home_root.path().join(".fabro")))
            .with_finish_callback(Arc::new(move || {
                callback_flag.store(true, Ordering::Release);
            })),
    )
    .await;

    configure_token_install(&app, "test-install-token").await;

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
    response_status(
        finish_response,
        StatusCode::ACCEPTED,
        "POST /install/finish",
    )
    .await;
    assert!(!callback_invoked.load(Ordering::Acquire));

    sleep(Duration::from_millis(650)).await;
    assert!(callback_invoked.load(Ordering::Acquire));
}

#[tokio::test]
async fn install_validation_endpoints_validate_credentials_and_github_token() {
    let llm_mock = MockServer::start_async().await;
    llm_mock
        .mock_async(|when, then| {
            when.method("GET").path("/v1/models");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    serde_json::to_string(&serde_json::json!({
                        "data": [
                            {
                                "id": "claude-sonnet-4-5",
                                "type": "model"
                            }
                        ]
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
    )
    .await;

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
    let llm_body = response_json(llm_response, StatusCode::OK, "POST /install/llm/test").await;
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
    let github_body = response_json(
        github_response,
        StatusCode::OK,
        "POST /install/github/token/test",
    )
    .await;
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
    )
    .await;

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
    response_status(
        server_response,
        StatusCode::NO_CONTENT,
        "PUT /install/server",
    )
    .await;

    let manifest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":{"kind":"personal"},"app_name":"Fabro Test","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let manifest_body = response_json(
        manifest_response,
        StatusCode::OK,
        "POST /install/github/app/manifest",
    )
    .await;
    assert_eq!(
        manifest_body["github_form_action"],
        "https://github.com/settings/apps/new"
    );
    assert_eq!(
        manifest_body["manifest"]["callback_urls"][0],
        "https://fabro.example.com/auth/callback/github"
    );

    let state = manifest_body["state"]
        .as_str()
        .expect("state should be present on manifest response")
        .to_owned();
    assert_eq!(
        manifest_body["manifest"]["redirect_url"],
        "https://fabro.example.com/install/github/app/redirect",
        "redirect_url must not carry a query string — GitHub rejects manifests whose redirect_url has one"
    );

    let callback_response = checked_response(
        app.clone()
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
            .unwrap(),
        StatusCode::FOUND,
        "GET /install/github/app/redirect?code=stub-code&state=...",
    )
    .await;
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
    let session_body =
        response_json(session_response, StatusCode::OK, "GET /install/session").await;
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
async fn github_app_manifest_retry_replaces_pending_and_preserves_prior_token_strategy() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;

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
    response_status(
        server_response,
        StatusCode::NO_CONTENT,
        "PUT /install/server",
    )
    .await;

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
    response_status(
        github_response,
        StatusCode::NO_CONTENT,
        "PUT /install/github/token",
    )
    .await;

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":{"kind":"personal"},"app_name":"Fabro Test","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let first_body = response_json(
        first_response,
        StatusCode::OK,
        "POST /install/github/app/manifest (initial)",
    )
    .await;
    let first_state = first_body["state"]
        .as_str()
        .expect("state should be present on manifest response")
        .to_owned();

    let retry_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":{"kind":"personal"},"app_name":"Fabro Retry","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let retry_body = response_json(
        retry_response,
        StatusCode::OK,
        "POST /install/github/app/manifest (retry)",
    )
    .await;
    let retry_state = retry_body["state"]
        .as_str()
        .expect("state should be present on retry manifest response")
        .to_owned();
    assert_ne!(
        first_state, retry_state,
        "retry must mint a fresh state token so the old callback is invalidated"
    );

    // A late callback using the now-discarded first-attempt state must not
    // complete the install.
    let stale_callback = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/install/github/app/redirect?code=stub-code&state={first_state}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    response_status(
        stale_callback,
        StatusCode::FOUND,
        "GET /install/github/app/redirect with stale state",
    )
    .await;

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
    let session_body =
        response_json(session_response, StatusCode::OK, "GET /install/session").await;
    assert_eq!(session_body["github"]["strategy"], "token");
    assert_eq!(session_body["github"]["username"], "brynary");
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
    )
    .await;

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
    response_status(
        server_response,
        StatusCode::NO_CONTENT,
        "PUT /install/server",
    )
    .await;

    let manifest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":{"kind":"personal"},"app_name":"Fabro Test","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let manifest_body = response_json(
        manifest_response,
        StatusCode::OK,
        "POST /install/github/app/manifest",
    )
    .await;
    let state = manifest_body["state"]
        .as_str()
        .expect("state should be present on manifest response")
        .to_owned();
    assert_eq!(
        manifest_body["manifest"]["redirect_url"],
        "https://fabro.example.com/install/github/app/redirect",
        "redirect_url must not carry a query string — GitHub rejects manifests whose redirect_url has one"
    );

    let wrong_state_response = checked_response(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/install/github/app/redirect?code=stub-code&state=wrong-state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::FOUND,
        "GET /install/github/app/redirect?code=stub-code&state=wrong-state",
    )
    .await;
    assert_eq!(
        wrong_state_response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/install/github?token=test-install-token&error=invalid-install-github-app-state")
    );
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
    let session_body =
        response_json(session_response, StatusCode::OK, "GET /install/session").await;
    assert!(session_body["github"].is_null());
    assert!(
        !session_body["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "github")
    );

    let missing_state_response = checked_response(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/install/github/app/redirect?code=stub-code")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::FOUND,
        "GET /install/github/app/redirect?code=stub-code",
    )
    .await;
    assert_eq!(
        missing_state_response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/install/github?token=test-install-token&error=missing-install-github-app-state")
    );
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
    response_status(
        valid_state_response,
        StatusCode::FOUND,
        "GET /install/github/app/redirect?code=stub-code&state=...",
    )
    .await;
    conversion_mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn github_app_redirect_exchange_failure_returns_to_wizard_and_keeps_pending_state() {
    let github_mock = MockServer::start_async().await;
    let conversion_mock = github_mock
        .mock_async(|when, then| {
            when.method("POST")
                .path("/app-manifests/stub-code/conversions");
            then.status(502).body("upstream exploded");
        })
        .await;
    let app = build_install_router(
        InstallAppState::for_test("test-install-token")
            .with_github_api_base_url(github_mock.url("")),
    )
    .await;

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
    response_status(
        server_response,
        StatusCode::NO_CONTENT,
        "PUT /install/server",
    )
    .await;

    let manifest_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/github/app/manifest")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"owner":{"kind":"personal"},"app_name":"Fabro Test","allowed_username":"octocat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let manifest_body = response_json(
        manifest_response,
        StatusCode::OK,
        "POST /install/github/app/manifest",
    )
    .await;
    let state = manifest_body["state"]
        .as_str()
        .expect("state should be present on manifest response")
        .to_owned();
    assert_eq!(
        manifest_body["manifest"]["redirect_url"],
        "https://fabro.example.com/install/github/app/redirect",
        "redirect_url must not carry a query string — GitHub rejects manifests whose redirect_url has one"
    );

    let callback_response = checked_response(
        app.clone()
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
            .unwrap(),
        StatusCode::FOUND,
        "GET /install/github/app/redirect?code=stub-code&state=...",
    )
    .await;
    assert_eq!(
        callback_response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some(
            "/install/github?token=test-install-token&error=github-app-manifest-conversion-failed"
        )
    );
    conversion_mock.assert_calls_async(1).await;

    let retry_response = app
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
    response_status(
        retry_response,
        StatusCode::FOUND,
        "GET /install/github/app/redirect?code=stub-code&state=...",
    )
    .await;
    conversion_mock.assert_calls_async(2).await;
}

#[tokio::test]
async fn install_server_rejects_trailing_slash_canonical_urls() {
    let app = build_install_router(InstallAppState::for_test("test-install-token")).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/server")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"canonical_url":"https://fabro.example.com/"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "PUT /install/server",
    )
    .await;
    assert_eq!(
        body["errors"][0]["detail"],
        "canonical_url must not end with a trailing slash"
    );
}

#[tokio::test]
async fn install_finish_failure_restores_settings_and_vault_but_leaves_env_keys() {
    let temp_dir = tempfile::tempdir().unwrap();
    let home_root = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    std::fs::write(&config_path, "_version = 1\n[project]\nname = \"keep\"\n").unwrap();

    let storage = Storage::new(temp_dir.path());
    let vault_path = storage.secrets_path();
    std::fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    std::fs::write(&vault_path, "{ not valid json").unwrap();
    let callback_invoked = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_invoked);

    let app = build_install_router(
        InstallAppState::for_test_with_paths("test-install-token", temp_dir.path(), &config_path)
            .with_home(Home::new(home_root.path().join(".fabro")))
            .with_finish_callback(Arc::new(move || {
                callback_flag.store(true, Ordering::Release);
            })),
    )
    .await;

    configure_token_install(&app, "test-install-token").await;

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
    let finish_body = response_json(
        finish_response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST /install/finish",
    )
    .await;
    assert!(
        finish_body["errors"][0]["detail"]
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

    let server_env = std::fs::read_to_string(storage.runtime_directory().env_path()).unwrap();
    assert!(server_env.contains("SESSION_SECRET="));
    assert!(server_env.contains("FABRO_DEV_TOKEN="));
    assert!(!callback_invoked.load(Ordering::Acquire));

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
    let session_body =
        response_json(session_response, StatusCode::OK, "GET /install/session").await;
    assert!(
        session_body["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "github")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn install_finish_failure_with_manual_credentials_does_not_leak_values() {
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
    ))
    .await;

    let access_key_id = "AKIA_FINISH_SHOULD_NOT_LEAK";
    let secret_access_key = "finish-secret-should-not-leak";

    put_install_server(&app, "test-install-token", "https://fabro.example.com").await;
    put_install_object_store(
        &app,
        "test-install-token",
        &format!(
            r#"{{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"access_key","access_key_id":"{access_key_id}","secret_access_key":"{secret_access_key}"}}"#
        ),
    )
    .await;
    put_install_llm(&app, "test-install-token").await;
    put_install_github_token(&app, "test-install-token", "brynary").await;

    let lines = Arc::new(StdMutex::new(Vec::new()));
    let subscriber = Registry::default().with(CaptureLayer {
        lines: Arc::clone(&lines),
    });
    let _guard = tracing::subscriber::set_default(subscriber);

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
    let finish_body = response_json(
        finish_response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST /install/finish",
    )
    .await;

    let rendered = finish_body.to_string();
    assert!(!rendered.contains(access_key_id));
    assert!(!rendered.contains(secret_access_key));

    let captured = lines.lock().unwrap().join("\n");
    assert!(
        captured.contains("install persistence failed"),
        "expected finish failure logs to be captured, got: {captured}"
    );
    assert!(!captured.contains(access_key_id));
    assert!(!captured.contains(secret_access_key));
}

#[tokio::test]
async fn install_finish_failure_reports_only_env_keys_actually_removed() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("settings.toml");
    std::fs::write(&config_path, "_version = 1\n[project]\nname = \"keep\"\n").unwrap();

    let storage = Storage::new(temp_dir.path());
    let env_path = storage.runtime_directory().env_path();
    std::fs::create_dir_all(env_path.parent().unwrap()).unwrap();
    std::fs::write(
        &env_path,
        format!(
            "#{OBJECT_STORE_MANAGED_COMMENT}\nAWS_SECRET_ACCESS_KEY=managed-secret\nKEEP_ME=1\n"
        ),
    )
    .unwrap();

    let vault_path = storage.secrets_path();
    std::fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    std::fs::write(&vault_path, "{ not valid json").unwrap();

    let app = build_install_router(InstallAppState::for_test_with_paths(
        "test-install-token",
        temp_dir.path(),
        &config_path,
    ))
    .await;

    put_install_server(&app, "test-install-token", "https://fabro.example.com").await;
    put_install_object_store(
        &app,
        "test-install-token",
        r#"{"provider":"s3","bucket":"fabro-data","region":"us-east-1","credential_mode":"runtime"}"#,
    )
    .await;
    put_install_llm(&app, "test-install-token").await;
    put_install_github_token(&app, "test-install-token", "brynary").await;

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
    let finish_body = response_json(
        finish_response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST /install/finish",
    )
    .await;

    assert_eq!(
        finish_body["removed_env_keys"]
            .as_array()
            .expect("removed_env_keys should be present"),
        &vec![serde_json::Value::String(
            "AWS_SECRET_ACCESS_KEY".to_string()
        )]
    );

    let server_env = std::fs::read_to_string(env_path).unwrap();
    assert!(!server_env.contains("AWS_SECRET_ACCESS_KEY=managed-secret"));
    assert!(server_env.contains("KEEP_ME=1"));
}

#[tokio::test]
async fn install_finish_failure_leaves_home_dev_token_mirror_written() {
    let temp_dir = tempfile::tempdir().unwrap();
    let home_root = tempfile::tempdir().unwrap();
    let home = Home::new(home_root.path().join(".fabro"));
    let config_path = temp_dir.path().join("settings.toml");
    std::fs::write(&config_path, "_version = 1\n[project]\nname = \"keep\"\n").unwrap();

    let storage = Storage::new(temp_dir.path());
    let vault_path = storage.secrets_path();
    std::fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    std::fs::write(&vault_path, "{ not valid json").unwrap();

    let app = build_install_router(
        InstallAppState::for_test_with_paths("test-install-token", temp_dir.path(), &config_path)
            .with_home(home.clone()),
    )
    .await;

    configure_token_install(&app, "test-install-token").await;

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
    response_status(
        finish_response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST /install/finish",
    )
    .await;

    let home_dev_token = dev_token::read_dev_token_file(&home.dev_token_path())
        .expect("home dev token should exist");
    let storage_dev_token =
        dev_token::read_dev_token_file(&storage.runtime_directory().dev_token_path())
            .expect("storage dev token should exist");
    assert_eq!(home_dev_token, storage_dev_token);

    let server_env = std::fs::read_to_string(storage.runtime_directory().env_path()).unwrap();
    assert!(server_env.contains(&format!("FABRO_DEV_TOKEN={home_dev_token}")));
}
