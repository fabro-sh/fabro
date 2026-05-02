use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
use std::sync::{Arc as StdArc, Mutex as StdMutex};

use axum::body::Body;
use axum::http::{Method, Request, header};
use chrono::{Duration as ChronoDuration, Utc};
use fabro_auth::{AuthCredential, AuthDetails};
use fabro_config::ServerSettingsBuilder;
use fabro_config::bind::Bind;
use fabro_interview::{AnswerValue, ControlInterviewer, Interviewer, Question};
use fabro_llm::types::{Message as LlmMessage, Request as LlmRequest};
use fabro_model::Provider;
use fabro_types::settings::ServerAuthMethod;
use fabro_types::{
    AttrValue, AuthMethod, CommandTermination, FailureCategory, FailureDetail, Graph,
    InterviewQuestionRecord, Outcome, QuestionType, RunBlobId, RunId, RunSpec, StageOutcome,
    SystemActorKind, fixtures,
};
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;
use tokio_stream::StreamExt as _;
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::{Event as TracingEvent, Subscriber, subscriber};
use tracing_subscriber::layer::Context as SubscriberContext;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, Registry};

use super::*;
use crate::github_webhooks::compute_signature;
use crate::jwt_auth::{AuthMode, ConfiguredAuth};
use crate::test_support::*;

const MINIMAL_DOT: &str = r#"digraph Test {
    graph [goal="Test"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    start -> exit
}"#;
const TEST_WEBHOOK_SECRET: &str = "webhook-secret";
const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";
const TEST_SESSION_SECRET: &str = "server-test-session-key-0123456789";
const TEST_JWT_ISSUER: &str = "https://fabro.example";
const WRONG_DEV_TOKEN: &str =
    "fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

fn manifest_run_defaults_from_toml(source: &str) -> fabro_config::RunLayer {
    let mut document: toml::Table = source.parse().expect("run defaults should parse");
    document
        .remove("run")
        .map(toml::Value::try_into::<fabro_config::RunLayer>)
        .transpose()
        .expect("run defaults should parse")
        .unwrap_or_default()
}

fn server_settings_from_toml(source: &str) -> ServerSettings {
    ServerSettingsBuilder::from_toml(source).expect("server settings should resolve")
}

fn resolved_runtime_settings_from_toml(source: &str) -> ResolvedAppStateSettings {
    resolved_runtime_settings_for_tests(
        server_settings_from_toml(source),
        manifest_run_defaults_from_toml(source),
    )
}

fn test_app_with() -> Router {
    let state = test_app_state();
    crate::test_support::build_test_router_with_options(
        state,
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions {
            static_asset_root: Some(spa_fixture_root()),
            ..RouterOptions::default()
        },
    )
}

fn spa_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spa")
}

fn test_app_with_scheduler(state: Arc<AppState>) -> Router {
    spawn_scheduler(Arc::clone(&state));
    crate::test_support::build_test_router(state)
}

fn test_app_state_with_isolated_storage() -> Arc<AppState> {
    let storage_dir = std::env::temp_dir().join(format!("fabro-server-test-{}", Ulid::new()));
    std::fs::create_dir_all(&storage_dir).expect("test storage dir should be creatable");
    let source = format!(
        r#"
_version = 1

[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token"]
"#,
        storage_dir.display()
    );

    test_app_state_with_options(
        server_settings_from_toml(&source),
        manifest_run_defaults_from_toml(&source),
        5,
    )
}

async fn body_json(body: Body) -> serde_json::Value {
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn openai_api_key_credential(key: &str) -> AuthCredential {
    AuthCredential {
        provider: Provider::OpenAi,
        details:  AuthDetails::ApiKey {
            key: key.to_string(),
        },
    }
}

fn openai_responses_payload(text: &str) -> serde_json::Value {
    json!({
        "id": "resp_1",
        "model": "gpt-5.4",
        "output": [
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": text
                    }
                ]
            }
        ],
        "status": "completed",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20
        }
    })
}

macro_rules! assert_status {
    ($response:expr, $expected:expr) => {
        fabro_test::assert_axum_status($response, $expected, concat!(file!(), ":", line!()))
    };
}

macro_rules! checked_response {
    ($response:expr, $expected:expr) => {
        fabro_test::expect_axum_status($response, $expected, concat!(file!(), ":", line!()))
    };
}

#[derive(Clone, Debug)]
struct CapturedTracingEvent {
    fields: Vec<(String, String)>,
}

#[derive(Default)]
struct CaptureVisitor {
    fields: Vec<(String, String)>,
}

impl Visit for CaptureVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .push((field.name().to_string(), format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
}

struct ServerLogCaptureLayer {
    events: StdArc<StdMutex<Vec<CapturedTracingEvent>>>,
}

impl<S: Subscriber> Layer<S> for ServerLogCaptureLayer {
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: SubscriberContext<'_, S>) {
        if !event
            .metadata()
            .target()
            .starts_with("fabro_server::server")
        {
            return;
        }
        let mut visitor = CaptureVisitor::default();
        event.record(&mut visitor);
        if visitor
            .fields
            .iter()
            .any(|(name, value)| name == "message" && value == "HTTP response")
        {
            self.events
                .lock()
                .expect("captured log events lock poisoned")
                .push(CapturedTracingEvent {
                    fields: visitor.fields,
                });
        }
    }
}

fn capture_server_logs() -> (
    tracing::dispatcher::DefaultGuard,
    StdArc<StdMutex<Vec<CapturedTracingEvent>>>,
) {
    let events = StdArc::new(StdMutex::new(Vec::new()));
    let subscriber = Registry::default().with(ServerLogCaptureLayer {
        events: StdArc::clone(&events),
    });
    let guard = subscriber::set_default(subscriber);
    (guard, events)
}

fn captured_field<'a>(event: &'a CapturedTracingEvent, name: &str) -> Option<&'a str> {
    event
        .fields
        .iter()
        .find_map(|(field_name, value)| (field_name == name).then_some(value.as_str()))
}

fn assert_log_field(event: &CapturedTracingEvent, name: &str, expected: &str) {
    let actual = captured_field(event, name)
        .unwrap_or_else(|| panic!("expected log field {name}; fields were {:?}", event.fields));
    let debug_expected = format!("{expected:?}");
    assert!(
        actual == expected || actual == debug_expected,
        "expected field {name} to be {expected:?}, got {actual:?}; fields were {:?}",
        event.fields
    );
}

fn assert_log_field_absent(event: &CapturedTracingEvent, name: &str) {
    assert!(
        captured_field(event, name).is_none(),
        "expected log field {name} to be absent; fields were {:?}",
        event.fields
    );
}

macro_rules! response_json {
    ($response:expr, $expected:expr) => {
        fabro_test::expect_axum_json($response, $expected, concat!(file!(), ":", line!()))
    };
}

macro_rules! response_bytes {
    ($response:expr, $expected:expr) => {
        fabro_test::expect_axum_bytes($response, $expected, concat!(file!(), ":", line!()))
    };
}

fn api(path: &str) -> String {
    format!("/api/v1{path}")
}

#[tokio::test(flavor = "current_thread")]
async fn http_log_omits_unset_optional_auth_fields() {
    let (_guard, events) = capture_server_logs();
    let app = test_app_with();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let events = events.lock().expect("captured log events").clone();
    assert_eq!(events.len(), 1);
    let field_names = events[0]
        .fields
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert!(field_names.contains(&"principal_kind"));
    assert!(field_names.contains(&"auth_status"));
    assert!(!field_names.contains(&"auth_error_code"));
    assert!(!field_names.contains(&"user_auth_method"));
    assert!(!field_names.contains(&"idp_issuer"));
    assert!(!field_names.contains(&"run_id"));
}

#[tokio::test(flavor = "current_thread")]
async fn http_log_records_user_principal_fields() {
    let (_state, app) = jwt_auth_app();
    let bearer = issue_test_user_jwt();
    let (_guard, events) = capture_server_logs();

    let response = app
        .oneshot(bearer_request(Method::GET, "/runs", &bearer, Body::empty()))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let events = events.lock().expect("captured log events").clone();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_log_field(event, "principal_kind", "user");
    assert_log_field(event, "auth_status", "authenticated");
    assert_log_field(event, "user_auth_method", "github");
    assert_log_field(event, "idp_issuer", "https://github.com");
    assert_log_field(event, "idp_subject", "12345");
    assert_log_field(event, "login", "octocat");
    assert_log_field_absent(event, "auth_error_code");
}

#[tokio::test(flavor = "current_thread")]
async fn http_log_records_worker_principal_fields() {
    let (_state, app) = jwt_auth_app();
    let user_bearer = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_bearer).await;
    let worker_bearer = issue_test_worker_token(&run_id);
    let (_guard, events) = capture_server_logs();

    let response = app
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/state"),
            &worker_bearer,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let events = events.lock().expect("captured log events").clone();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_log_field(event, "principal_kind", "worker");
    assert_log_field(event, "auth_status", "authenticated");
    assert_log_field(event, "run_id", &run_id.to_string());
    assert_log_field_absent(event, "auth_error_code");
}

#[tokio::test(flavor = "current_thread")]
async fn http_log_records_webhook_principal_fields() {
    let body = br#"{"repository":{"full_name":"owner/repo"},"action":"opened"}"#;
    let signature = compute_signature(TEST_WEBHOOK_SECRET.as_bytes(), body);
    let app = webhook_test_app(dev_token_auth_mode());
    let (_guard, events) = capture_server_logs();

    let response = app
        .oneshot(webhook_request(Some(&signature), None, body))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let events = events.lock().expect("captured log events").clone();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_log_field(event, "principal_kind", "webhook");
    assert_log_field(event, "auth_status", "authenticated");
    assert_log_field(event, "delivery_id", "delivery-1");
    assert_log_field_absent(event, "auth_error_code");
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Test helper mirrors the public build_router convenience API."
)]
fn webhook_test_app(auth_mode: AuthMode) -> Router {
    let secret = TEST_WEBHOOK_SECRET.to_string();
    let state = test_app_state_with_env_lookup_and_server_secret_env(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
        &HashMap::from([(WEBHOOK_SECRET_ENV.to_string(), secret)]),
    );
    build_router_with_options(
        state,
        &auth_mode,
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions {
            web_enabled: false,
            ..RouterOptions::default()
        },
    )
}

fn webhook_request(
    signature: Option<&str>,
    authorization: Option<&str>,
    body: &[u8],
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(api("/webhooks/github"))
        .header("x-github-delivery", "delivery-1")
        .header("x-github-event", "pull_request");
    if let Some(sig) = signature {
        builder = builder.header("x-hub-signature-256", sig);
    }
    if let Some(value) = authorization {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    builder.body(Body::from(body.to_vec())).unwrap()
}

fn dev_token_auth_mode() -> AuthMode {
    AuthMode::Enabled(ConfiguredAuth {
        methods:    vec![ServerAuthMethod::DevToken],
        dev_token:  Some(TEST_DEV_TOKEN.to_string()),
        jwt_key:    None,
        jwt_issuer: None,
    })
}

fn jwt_auth_mode() -> AuthMode {
    AuthMode::Enabled(ConfiguredAuth {
        methods:    vec![ServerAuthMethod::Github],
        dev_token:  None,
        jwt_key:    Some(
            auth::derive_jwt_key(TEST_SESSION_SECRET.as_bytes())
                .expect("test JWT key should derive"),
        ),
        jwt_issuer: Some(TEST_JWT_ISSUER.to_string()),
    })
}

fn jwt_auth_state() -> Arc<AppState> {
    test_app_state_with_session_key(
        default_test_server_settings(),
        RunLayer::default(),
        Some(TEST_SESSION_SECRET),
    )
}

fn jwt_auth_app() -> (Arc<AppState>, Router) {
    let state = jwt_auth_state();
    let app = build_router(Arc::clone(&state), jwt_auth_mode());
    (state, app)
}

fn test_user_subject() -> auth::JwtSubject {
    auth::JwtSubject {
        identity:    fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
        login:       "octocat".to_string(),
        name:        "The Octocat".to_string(),
        email:       "octocat@example.com".to_string(),
        avatar_url:  "https://example.com/octocat.png".to_string(),
        user_url:    "https://github.com/octocat".to_string(),
        auth_method: AuthMethod::Github,
    }
}

fn issue_test_user_jwt() -> String {
    let key =
        auth::derive_jwt_key(TEST_SESSION_SECRET.as_bytes()).expect("test JWT key should derive");
    auth::issue(
        &key,
        TEST_JWT_ISSUER,
        &test_user_subject(),
        ChronoDuration::minutes(10),
    )
}

fn issue_test_worker_token(run_id: &RunId) -> String {
    let keys = WorkerTokenKeys::from_master_secret(TEST_SESSION_SECRET.as_bytes())
        .expect("worker keys should derive");
    issue_worker_token(&keys, run_id).expect("worker token should issue")
}

async fn create_run_with_bearer(app: &Router, bearer: &str) -> RunId {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(manifest_body(MINIMAL_DOT))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    body["id"].as_str().unwrap().parse().unwrap()
}

fn bearer_request(method: Method, path: &str, bearer: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(body)
        .unwrap()
}

fn canonical_origin_settings(url: &str) -> ServerSettings {
    server_settings_from_toml(&format!(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "{url}"
"#
    ))
}

fn canonical_host_test_app() -> Router {
    let state = test_app_state_with_options(
        canonical_origin_settings("http://127.0.0.1:32276"),
        RunLayer::default(),
        5,
    );
    crate::test_support::build_test_router_with_options(
        state,
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions::default(),
    )
}

#[tokio::test]
async fn router_redirects_web_page_requests_to_canonical_host() {
    let app = canonical_host_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/login")
                .header(header::HOST, "localhost:32276")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let response = checked_response!(response, StatusCode::PERMANENT_REDIRECT).await;
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "http://127.0.0.1:32276/login"
    );
}

#[tokio::test]
async fn router_does_not_redirect_api_requests_to_canonical_host() {
    let app = canonical_host_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(api("/openapi.json"))
                .header(header::HOST, "localhost:32276")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status!(response, StatusCode::OK).await;
}

#[test]
fn replace_settings_rejects_invalid_canonical_origin_and_keeps_previous_settings() {
    for invalid in [
        "",
        "/relative/path",
        "ftp://fabro.example.com",
        "http://0.0.0.0:32276",
    ] {
        let state = test_app_state_with_env_lookup(
            canonical_origin_settings("http://valid.example.com"),
            RunLayer::default(),
            5,
            {
                let invalid = invalid.to_string();
                move |name| (name == "FABRO_WEB_URL").then(|| invalid.clone())
            },
        );

        let err = state
            .replace_runtime_settings(resolved_runtime_settings_from_toml(
                r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "{{ env.FABRO_WEB_URL }}"
"#,
            ))
            .expect_err("invalid canonical origin should be rejected");
        assert!(
            err.to_string()
                .contains("server.web.url is required and must be an absolute http(s) URL"),
            "unexpected error for {invalid}: {err}"
        );
        assert_eq!(
            state.canonical_origin().unwrap(),
            "http://valid.example.com".to_string()
        );
    }
}

#[test]
fn replace_settings_updates_layer_and_typed_server_settings() {
    let state = test_app_state_with_options(
        server_settings_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "http://old.example.com"

[server.storage]
root = "/srv/old"
"#,
        ),
        manifest_run_defaults_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "http://old.example.com"

[server.storage]
root = "/srv/old"
"#,
        ),
        5,
    );

    let updated = r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "http://new.example.com"

[run.execution]
mode = "dry_run"

[server.storage]
root = "/srv/new"
"#;

    state
        .replace_runtime_settings(resolved_runtime_settings_from_toml(updated))
        .expect("valid settings should replace current state");

    assert_eq!(state.canonical_origin().unwrap(), "http://new.example.com");
    assert_eq!(
        state.server_settings().server.storage.root.as_source(),
        "/srv/new"
    );
    assert_eq!(
        state
            .manifest_run_settings()
            .expect("manifest run settings should resolve")
            .execution
            .mode,
        RunMode::DryRun
    );
    let manifest_run_defaults = state.manifest_run_defaults();
    assert_eq!(
        manifest_run_defaults
            .execution
            .as_ref()
            .and_then(|execution| execution.mode),
        Some(RunMode::DryRun)
    );
}

#[test]
fn replace_settings_caches_invalid_manifest_run_settings_tolerantly() {
    let state = test_app_state_with_options(
        server_settings_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "http://old.example.com"
"#,
        ),
        manifest_run_defaults_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "http://old.example.com"
"#,
        ),
        5,
    );

    let updated = r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "http://new.example.com"

[run.sandbox]
provider = "invalid-provider"
"#;

    state
        .replace_runtime_settings(resolved_runtime_settings_from_toml(updated))
        .expect("invalid run defaults should not block replace");

    assert_eq!(state.canonical_origin().unwrap(), "http://new.example.com");
    assert!(
        state.manifest_run_settings().is_err(),
        "manifest run settings should stay tolerant for invalid defaults"
    );
}

#[test]
fn system_features_use_dense_server_and_manifest_defaults() {
    let source = r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[features]
session_sandboxes = true

[run.execution]
retros = false
"#;
    let server_settings = server_settings_from_toml(source);
    let manifest_run_settings = resolve_manifest_run_settings(
        &run_manifest::manifest_run_defaults(Some(&manifest_run_defaults_from_toml(source))),
    );
    let features = system_features(&server_settings, &manifest_run_settings);

    assert_eq!(features.session_sandboxes, Some(true));
    assert_eq!(features.retros, Some(false));
}

#[test]
fn system_features_default_retros_when_manifest_run_settings_do_not_resolve() {
    let source = r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[features]
session_sandboxes = true

[run.sandbox]
provider = "invalid-provider"
"#;
    let server_settings = server_settings_from_toml(source);
    let manifest_run_settings = resolve_manifest_run_settings(
        &run_manifest::manifest_run_defaults(Some(&manifest_run_defaults_from_toml(source))),
    );
    let features = system_features(&server_settings, &manifest_run_settings);

    assert_eq!(features.session_sandboxes, Some(true));
    assert_eq!(features.retros, Some(false));
}

#[test]
fn system_sandbox_provider_uses_manifest_defaults() {
    let source = r#"
_version = 1

[run.sandbox]
provider = "daytona"
"#;
    let manifest_run_settings = resolve_manifest_run_settings(
        &run_manifest::manifest_run_defaults(Some(&manifest_run_defaults_from_toml(source))),
    );

    assert_eq!(system_sandbox_provider(&manifest_run_settings), "daytona");
}

#[test]
fn system_sandbox_provider_defaults_when_manifest_run_settings_do_not_resolve() {
    let source = r#"
_version = 1

[run.sandbox]
provider = "invalid-provider"
"#;
    let manifest_run_settings = resolve_manifest_run_settings(
        &run_manifest::manifest_run_defaults(Some(&manifest_run_defaults_from_toml(source))),
    );

    assert_eq!(
        system_sandbox_provider(&manifest_run_settings),
        SandboxProvider::default().to_string()
    );
}

#[test]
fn clone_sandbox_credentials_are_available_for_clone_based_providers() {
    assert!(clone_sandbox_can_use_github_credentials("docker"));
    assert!(clone_sandbox_can_use_github_credentials("daytona"));
    assert!(!clone_sandbox_can_use_github_credentials("local"));
}

#[tokio::test]
async fn create_secret_stores_file_secret_and_excludes_it_from_snapshot() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "/tmp/test.pem",
                "value": "pem-data",
                "type": "file",
                "description": "Test certificate",
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["name"], "/tmp/test.pem");
    assert_eq!(body["type"], "file");
    assert_eq!(body["description"], "Test certificate");

    let vault = state.vault.read().await;
    assert!(!vault.snapshot().contains_key("/tmp/test.pem"));
    assert_eq!(vault.file_secrets(), vec![(
        "/tmp/test.pem".to_string(),
        "pem-data".to_string()
    )]);
}

#[tokio::test]
async fn github_webhook_rejects_missing_signature() {
    let app = webhook_test_app(crate::test_support::test_auth_mode());
    let body = br#"{"action":"opened"}"#;

    let response = app
        .oneshot(webhook_request(None, None, body))
        .await
        .unwrap();
    assert_status!(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn github_webhook_rejects_signature_signed_with_wrong_secret() {
    let app = webhook_test_app(crate::test_support::test_auth_mode());
    let body = br#"{"action":"opened"}"#;
    let bad_signature = compute_signature(b"wrong-secret", body);

    let response = app
        .oneshot(webhook_request(Some(&bad_signature), None, body))
        .await
        .unwrap();
    assert_status!(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn github_webhook_accepts_valid_signature_when_auth_disabled() {
    let body = br#"{"repository":{"full_name":"owner/repo"},"action":"opened"}"#;
    let signature = compute_signature(TEST_WEBHOOK_SECRET.as_bytes(), body);
    let app = webhook_test_app(crate::test_support::test_auth_mode());

    let response = app
        .oneshot(webhook_request(Some(&signature), None, body))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;
}

#[tokio::test]
async fn github_webhook_accepts_valid_signature_without_bearer_token() {
    let body = br#"{"repository":{"full_name":"owner/repo"},"action":"opened"}"#;
    let signature = compute_signature(TEST_WEBHOOK_SECRET.as_bytes(), body);
    let app = webhook_test_app(dev_token_auth_mode());

    let response = app
        .oneshot(webhook_request(Some(&signature), None, body))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;
}

#[tokio::test]
async fn github_webhook_accepts_valid_signature_with_wrong_bearer_token() {
    let body = br#"{"repository":{"full_name":"owner/repo"},"action":"opened"}"#;
    let signature = compute_signature(TEST_WEBHOOK_SECRET.as_bytes(), body);
    let app = webhook_test_app(dev_token_auth_mode());

    let response = app
        .oneshot(webhook_request(
            Some(&signature),
            Some(&format!("Bearer {WRONG_DEV_TOKEN}")),
            body,
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;
}

#[tokio::test]
async fn create_secret_stores_valid_credential_entries() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let credential = fabro_auth::AuthCredential {
        provider: Provider::OpenAi,
        details:  fabro_auth::AuthDetails::CodexOAuth {
            tokens:     fabro_auth::OAuthTokens {
                access_token:  "access".to_string(),
                refresh_token: Some("refresh".to_string()),
                expires_at:    chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            },
            config:     fabro_auth::OAuthConfig {
                auth_url:     "https://auth.openai.com".to_string(),
                token_url:    "https://auth.openai.com/oauth/token".to_string(),
                client_id:    "client".to_string(),
                scopes:       vec!["openid".to_string()],
                redirect_uri: Some("https://auth.openai.com/deviceauth/callback".to_string()),
                use_pkce:     true,
            },
            account_id: Some("acct_123".to_string()),
        },
    };

    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "openai_codex",
                "value": serde_json::to_string(&credential).unwrap(),
                "type": "credential"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::OK).await;
    let listed = state.vault.read().await.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "openai_codex");
    assert_eq!(listed[0].secret_type, SecretType::Credential);
    assert!(state.vault.read().await.get("openai_codex").is_some());
}

#[tokio::test]
async fn resolve_llm_client_reads_openai_codex_credential_from_vault() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    state
        .vault
        .write()
        .await
        .set(
            "openai_codex",
            &serde_json::to_string(&openai_api_key_credential("vault-openai-key")).unwrap(),
            SecretType::Credential,
            None,
        )
        .unwrap();

    let llm_result = state.resolve_llm_client().await.unwrap();

    assert_eq!(llm_result.client.provider_names(), vec!["openai"]);
    assert!(llm_result.auth_issues.is_empty());
}

struct FailingCredentialSource;

#[async_trait::async_trait]
impl CredentialSource for FailingCredentialSource {
    async fn resolve(&self) -> anyhow::Result<fabro_auth::ResolvedCredentials> {
        Err(anyhow::Error::new(std::io::Error::other("credential leaf"))
            .context("credential source context"))
    }

    async fn configured_providers(&self) -> Vec<Provider> {
        Vec::new()
    }
}

#[tokio::test]
async fn resolve_llm_client_from_source_preserves_credential_source_chain() {
    let Err(err) = resolve_llm_client_from_source(&FailingCredentialSource).await else {
        panic!("expected credential resolution to fail");
    };
    let chain = err.chain().map(ToString::to_string).collect::<Vec<_>>();

    assert!(
        chain
            .iter()
            .any(|cause| cause == "credential source context"),
        "expected context in chain, got {chain:#?}"
    );
    assert!(
        chain.iter().any(|cause| cause == "credential leaf"),
        "expected source in chain, got {chain:#?}"
    );
}

#[tokio::test]
async fn llm_source_configured_providers_reads_openai_codex_from_vault() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    state
        .vault
        .write()
        .await
        .set(
            "openai_codex",
            &serde_json::to_string(&openai_api_key_credential("vault-openai-key")).unwrap(),
            SecretType::Credential,
            None,
        )
        .unwrap();

    assert_eq!(state.llm_source.configured_providers().await, vec![
        Provider::OpenAi
    ]);
}

#[tokio::test]
async fn resolve_llm_client_uses_env_lookup_for_openai_settings() {
    let server = MockServer::start_async().await;
    let response_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/responses")
                .header("authorization", "Bearer vault-openai-key")
                .header("OpenAI-Organization", "env-org");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(openai_responses_payload("hello from env lookup"));
        })
        .await;
    let base_url = server.url("/v1");
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        move |name| match name {
            "OPENAI_BASE_URL" => Some(base_url.clone()),
            "OPENAI_ORG_ID" => Some("env-org".to_string()),
            _ => None,
        },
    );
    state
        .vault
        .write()
        .await
        .set(
            "openai_codex",
            &serde_json::to_string(&openai_api_key_credential("vault-openai-key")).unwrap(),
            SecretType::Credential,
            None,
        )
        .unwrap();

    let llm_result = state.resolve_llm_client().await.unwrap();
    let response = llm_result
        .client
        .complete(&LlmRequest {
            model:            "gpt-5.4".to_string(),
            messages:         vec![LlmMessage::user("Hello")],
            provider:         Some("openai".to_string()),
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       None,
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        })
        .await
        .unwrap();

    assert_eq!(response.text(), "hello from env lookup");
    response_mock.assert_async().await;
}

#[tokio::test]
async fn list_secrets_includes_credential_metadata() {
    let state = test_app_state();
    {
        let mut vault = state.vault.write().await;
        vault
            .set(
                "anthropic",
                "{\"provider\":\"anthropic\"}",
                SecretType::Credential,
                Some("saved auth"),
            )
            .unwrap();
    }
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/secrets"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be an array");
    let entry = data
        .iter()
        .find(|entry| entry["name"] == "anthropic")
        .expect("credential metadata should be listed");
    assert_eq!(entry["type"], "credential");
    assert_eq!(entry["description"], "saved auth");
    assert!(entry.get("updated_at").is_some());
    assert!(entry.get("value").is_none());
}

#[tokio::test]
async fn create_secret_rejects_invalid_credential_json() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "openai_codex",
                "value": "{not-json",
                "type": "credential"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn create_secret_rejects_wrong_credential_name() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "openai",
                "value": serde_json::to_string(&serde_json::json!({
                    "provider": "openai",
                    "type": "codex_oauth",
                    "tokens": {
                        "access_token": "access",
                        "refresh_token": "refresh",
                        "expires_at": "2030-01-01T00:00:00Z"
                    },
                    "config": {
                        "auth_url": "https://auth.openai.com",
                        "token_url": "https://auth.openai.com/oauth/token",
                        "client_id": "client",
                        "scopes": ["openid"],
                        "redirect_uri": "https://auth.openai.com/deviceauth/callback",
                        "use_pkce": true
                    }
                }))
                .unwrap(),
                "type": "credential"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn delete_secret_by_name_removes_file_secret() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let create_req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "/tmp/test.pem",
                "value": "pem-data",
                "type": "file",
            }))
            .unwrap(),
        ))
        .unwrap();
    let create_response = app.clone().oneshot(create_req).await.unwrap();
    assert_status!(create_response, StatusCode::OK).await;

    let delete_req = Request::builder()
        .method("DELETE")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "/tmp/test.pem",
            }))
            .unwrap(),
        ))
        .unwrap();

    let delete_response = app.oneshot(delete_req).await.unwrap();
    assert_status!(delete_response, StatusCode::NO_CONTENT).await;
    assert!(state.vault.read().await.list().is_empty());
}

#[test]
fn server_secrets_resolve_process_env_before_server_env() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("server.env"),
        "SESSION_SECRET=file-value\nGITHUB_APP_CLIENT_SECRET=file-client\n",
    )
    .unwrap();

    let secrets = ServerSecrets::load(
        dir.path().join("server.env"),
        HashMap::from([("SESSION_SECRET".to_string(), "env-value".to_string())]),
    )
    .unwrap();

    assert_eq!(secrets.get("SESSION_SECRET").as_deref(), Some("env-value"));
    assert_eq!(
        secrets.get("GITHUB_APP_CLIENT_SECRET").as_deref(),
        Some("file-client")
    );
}

#[cfg(unix)]
#[test]
fn worker_command_always_sets_worker_token_env() {
    let github_only = tempfile::tempdir().unwrap();
    let github_state =
        worker_command_test_state(github_only.path(), &["github"], Some(TEST_DEV_TOKEN));
    let github_run_id = RunId::new();
    let github_cmd = worker_command(
        github_state.as_ref(),
        github_run_id,
        RunExecutionMode::Start,
        github_only.path(),
    )
    .unwrap();
    assert!(matches!(
        command_env_value(&github_cmd, "FABRO_WORKER_TOKEN"),
        EnvOverride::Set(_)
    ));
    assert_eq!(
        command_env_value(&github_cmd, "FABRO_DEV_TOKEN"),
        EnvOverride::Unchanged
    );
    let github_args = github_cmd
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        !github_args
            .iter()
            .any(|arg| arg == "--artifact-upload-token")
    );
    assert!(!github_args.iter().any(|arg| arg == "--worker-token"));
    let EnvOverride::Set(github_token) = command_env_value(&github_cmd, "FABRO_WORKER_TOKEN")
    else {
        panic!("worker token should be set");
    };
    let github_keys = WorkerTokenKeys::from_master_secret(TEST_SESSION_SECRET.as_bytes())
        .expect("worker keys should derive");
    let github_claims = jsonwebtoken::decode::<crate::worker_token::WorkerTokenClaims>(
        &github_token,
        github_keys.decoding_key(),
        github_keys.validation(),
    )
    .expect("github worker token should decode")
    .claims;
    assert_eq!(github_claims.run_id, github_run_id.to_string());

    let dev_token = tempfile::tempdir().unwrap();
    let dev_token_state =
        worker_command_test_state(dev_token.path(), &["dev-token"], Some(TEST_DEV_TOKEN));
    let dev_token_run_id = RunId::new();
    let dev_token_cmd = worker_command(
        dev_token_state.as_ref(),
        dev_token_run_id,
        RunExecutionMode::Start,
        dev_token.path(),
    )
    .unwrap();
    assert!(matches!(
        command_env_value(&dev_token_cmd, "FABRO_WORKER_TOKEN"),
        EnvOverride::Set(_)
    ));
    assert_eq!(
        command_env_value(&dev_token_cmd, "FABRO_DEV_TOKEN"),
        EnvOverride::Unchanged
    );
    let EnvOverride::Set(dev_worker_token) =
        command_env_value(&dev_token_cmd, "FABRO_WORKER_TOKEN")
    else {
        panic!("worker token should be set");
    };
    let dev_claims = jsonwebtoken::decode::<crate::worker_token::WorkerTokenClaims>(
        &dev_worker_token,
        github_keys.decoding_key(),
        github_keys.validation(),
    )
    .expect("dev-token worker token should decode")
    .claims;
    assert_eq!(dev_claims.run_id, dev_token_run_id.to_string());
}

#[cfg(unix)]
#[test]
fn worker_command_forwards_github_app_private_key_from_server_secrets() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state_with_extra_config_and_env_lookup(
        storage_dir.path(),
        &["dev-token"],
        Some(TEST_DEV_TOKEN),
        "",
        &[(EnvVars::GITHUB_APP_PRIVATE_KEY, "test-private-key")],
        |_| None,
    );
    let cmd = worker_command(
        state.as_ref(),
        RunId::new(),
        RunExecutionMode::Start,
        storage_dir.path(),
    )
    .unwrap();

    assert_eq!(
        command_env_value(&cmd, EnvVars::GITHUB_APP_PRIVATE_KEY),
        EnvOverride::Set("test-private-key".to_string())
    );
}

#[cfg(unix)]
#[test]
fn worker_command_omits_github_app_private_key_when_unset() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state(storage_dir.path(), &["dev-token"], Some(TEST_DEV_TOKEN));
    let cmd = worker_command(
        state.as_ref(),
        RunId::new(),
        RunExecutionMode::Start,
        storage_dir.path(),
    )
    .unwrap();

    assert_eq!(
        command_env_value(&cmd, EnvVars::GITHUB_APP_PRIVATE_KEY),
        EnvOverride::Unchanged
    );
}

#[cfg(unix)]
#[test]
fn worker_command_sets_fabro_log_from_server_logging_config() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state_with_extra_config(
        storage_dir.path(),
        &["dev-token"],
        Some(TEST_DEV_TOKEN),
        r#"
[server.logging]
level = "debug"
"#,
    );
    let run_id = RunId::new();

    let cmd = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Start,
        storage_dir.path(),
    )
    .unwrap();

    assert_eq!(
        command_env_value(&cmd, EnvVars::FABRO_LOG),
        EnvOverride::Set("debug".to_string())
    );
}

#[cfg(unix)]
#[test]
fn worker_command_sets_fabro_log_destination_from_server_logging_config() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state_with_extra_config(
        storage_dir.path(),
        &["dev-token"],
        Some(TEST_DEV_TOKEN),
        r#"
[server.logging]
destination = "stdout"
"#,
    );
    let run_id = RunId::new();

    let cmd = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Start,
        storage_dir.path(),
    )
    .unwrap();

    assert_eq!(
        command_env_value(&cmd, EnvVars::FABRO_LOG_DESTINATION),
        EnvOverride::Set("stdout".to_string())
    );
}

#[cfg(unix)]
#[test]
fn worker_command_env_log_destination_overrides_server_logging_config() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state_with_extra_config_and_env_lookup(
        storage_dir.path(),
        &["dev-token"],
        Some(TEST_DEV_TOKEN),
        r#"
[server.logging]
destination = "file"
"#,
        &[],
        |name| (name == EnvVars::FABRO_LOG_DESTINATION).then(|| "stdout".to_string()),
    );
    let run_id = RunId::new();

    let cmd = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Start,
        storage_dir.path(),
    )
    .unwrap();

    assert_eq!(
        command_env_value(&cmd, EnvVars::FABRO_LOG_DESTINATION),
        EnvOverride::Set("stdout".to_string())
    );
}

#[cfg(unix)]
#[test]
fn worker_command_rejects_invalid_env_log_destination() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state_with_extra_config_and_env_lookup(
        storage_dir.path(),
        &["dev-token"],
        Some(TEST_DEV_TOKEN),
        r#"
[server.logging]
destination = "file"
"#,
        &[],
        |name| (name == EnvVars::FABRO_LOG_DESTINATION).then(|| "stdot".to_string()),
    );
    let run_id = RunId::new();

    let Err(err) = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Start,
        storage_dir.path(),
    ) else {
        panic!("invalid env destination should fail");
    };

    let message = err.to_string();
    assert!(message.contains(EnvVars::FABRO_LOG_DESTINATION));
    assert!(message.contains("stdot"));
}

#[test]
fn build_app_state_requires_session_secret_for_worker_tokens() {
    let server_settings = server_settings_from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
    );
    let (store, artifact_store) = test_store_bundle();
    let vault_path = test_secret_store_path();
    let server_env_path = vault_path.with_file_name("server.env");
    let Err(err) = build_app_state(AppStateConfig {
        resolved_settings: resolved_runtime_settings_for_tests(
            server_settings,
            RunLayer::default(),
        ),
        registry_factory_override: None,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        vault_path,
        server_secrets: ServerSecrets::load(server_env_path, HashMap::new()).unwrap(),
        env_lookup: default_env_lookup(),
        github_api_base_url: None,
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
    }) else {
        panic!("build_app_state should require SESSION_SECRET")
    };

    assert!(err.to_string().contains(
        "Fabro server refuses to start: auth is configured but SESSION_SECRET is not set."
    ));
}

fn worker_command_test_state(
    storage_dir: &Path,
    methods: &[&str],
    dev_token: Option<&str>,
) -> Arc<AppState> {
    worker_command_test_state_with_extra_config(storage_dir, methods, dev_token, "")
}

fn worker_command_test_state_with_extra_config(
    storage_dir: &Path,
    methods: &[&str],
    dev_token: Option<&str>,
    extra_config: &str,
) -> Arc<AppState> {
    worker_command_test_state_with_extra_config_and_env_lookup(
        storage_dir,
        methods,
        dev_token,
        extra_config,
        &[],
        |_| None,
    )
}

fn worker_command_test_state_with_extra_config_and_env_lookup(
    storage_dir: &Path,
    methods: &[&str],
    dev_token: Option<&str>,
    extra_config: &str,
    extra_server_secrets: &[(&str, &str)],
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
) -> Arc<AppState> {
    let dev_token = dev_token.map(str::to_owned);
    std::fs::create_dir_all(storage_dir).unwrap();
    let source = format!(
        r#"
_version = 1

[server.storage]
root = "{}"

[server.auth]
methods = [{}]

[server.auth.github]
allowed_usernames = ["octocat"]
{extra_config}
"#,
        storage_dir.display(),
        methods
            .iter()
            .map(|method| format!("\"{method}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    ServerDaemon::new(
        std::process::id(),
        Bind::Tcp("127.0.0.1:32276".parse::<std::net::SocketAddr>().unwrap()),
        runtime_directory.log_path(),
    )
    .write(&runtime_directory)
    .unwrap();

    let mut server_secret_env: HashMap<String, String> = dev_token
        .map(|token| HashMap::from([("FABRO_DEV_TOKEN".to_string(), token)]))
        .unwrap_or_default();
    for (key, value) in extra_server_secrets {
        server_secret_env.insert((*key).to_string(), (*value).to_string());
    }
    test_app_state_with_env_lookup_and_server_secret_env(
        server_settings_from_toml(&source),
        manifest_run_defaults_from_toml(&source),
        5,
        env_lookup,
        &server_secret_env,
    )
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum EnvOverride {
    Unchanged,
    Removed,
    Set(String),
}

#[cfg(unix)]
fn command_env_value(cmd: &Command, key: &str) -> EnvOverride {
    cmd.as_std()
        .get_envs()
        .find_map(|(name, value)| {
            (name.to_str() == Some(key)).then(|| match value {
                Some(value) => EnvOverride::Set(value.to_string_lossy().into_owned()),
                None => EnvOverride::Removed,
            })
        })
        .unwrap_or(EnvOverride::Unchanged)
}

#[tokio::test]
async fn subprocess_answer_transport_cancel_run_enqueues_cancel_message() {
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let transport = RunAnswerTransport::Subprocess { control_tx };

    transport.cancel_run().await.unwrap();

    assert_eq!(
        control_rx.recv().await,
        Some(WorkerControlEnvelope::cancel_run())
    );
}

#[tokio::test]
async fn in_process_answer_transport_cancel_run_cancels_pending_interviews() {
    let interviewer = Arc::new(ControlInterviewer::new());
    let transport = RunAnswerTransport::InProcess {
        interviewer: Arc::clone(&interviewer),
    };
    let mut question = Question::new("Approve?", QuestionType::YesNo);
    question.id = "q-1".to_string();
    let ask_interviewer = Arc::clone(&interviewer);
    let answer_task = tokio::spawn(async move { ask_interviewer.ask(question).await });
    tokio::task::yield_now().await;

    transport.cancel_run().await.unwrap();

    let answer = answer_task.await.unwrap().answer;
    assert_eq!(answer.value, AnswerValue::Cancelled);
}

fn manifest_json(target_path: &str, dot_source: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "cwd": "/tmp",
        "target": {
            "identifier": target_path,
            "path": target_path,
        },
        "workflows": {
            target_path: {
                "source": dot_source,
                "files": {},
            },
        },
    })
}

fn minimal_manifest_json(dot_source: &str) -> serde_json::Value {
    manifest_json("workflow.fabro", dot_source)
}

fn manifest_body(dot_source: &str) -> Body {
    Body::from(serde_json::to_string(&minimal_manifest_json(dot_source)).unwrap())
}

fn manifest_body_for(target_path: &str, dot_source: &str) -> Body {
    Body::from(serde_json::to_string(&manifest_json(target_path, dot_source)).unwrap())
}

async fn create_run(app: &Router, dot_source: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(dot_source))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn validate_endpoint_returns_workflow_summary_without_preflight_checks() {
    let app = test_app_with();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/validate"))
                .header("content-type", "application/json")
                .body(manifest_body(MINIMAL_DOT))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["ok"], true);
    assert_eq!(body["workflow"]["name"], "Test");
    assert_eq!(body["workflow"]["nodes"], 2);
    assert_eq!(body["workflow"]["edges"], 1);
    assert!(body.get("checks").is_none());
}

async fn create_run_for_target(app: &Router, target_path: &str, dot_source: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body_for(target_path, dot_source))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    body["id"].as_str().unwrap().to_string()
}

fn named_workflow_dot(name: &str, goal: &str) -> String {
    format!(
        r#"digraph {name} {{
    graph [goal="{goal}"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    start -> exit
}}"#
    )
}

fn multipart_body(
    boundary: &str,
    manifest: &serde_json::Value,
    files: &[(&str, &str, &[u8])],
) -> Body {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"manifest\"\r\n");
    body.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    body.extend_from_slice(serde_json::to_string(manifest).unwrap().as_bytes());
    body.extend_from_slice(b"\r\n");

    for (part, filename, bytes) in files {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{part}\"; filename=\"{filename}\"\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Body::from(body)
}

/// Create a run via POST /runs, then start it via POST /runs/{id}/start.
/// Returns the run_id string.
async fn create_and_start_run(app: &Router, dot_source: &str) -> String {
    let run_id = create_run(app, dot_source).await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap();

    run_id
}

async fn create_durable_run_with_events(
    state: &Arc<AppState>,
    run_id: RunId,
    events: &[workflow_event::Event],
) {
    let run_store = state.store.create_run(&run_id).await.unwrap();
    for event in events {
        workflow_event::append_event(&run_store, &run_id, event)
            .await
            .unwrap();
    }
}

fn stage_status<'a>(body: &'a serde_json::Value, id: &str) -> &'a str {
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stage| stage["id"] == id)
        .and_then(|stage| stage["status"].as_str())
        .unwrap()
}

#[tokio::test]
async fn list_run_stages_projects_retrying_until_completion() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::StageStarted {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        1,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 3,
        },
        workflow_event::Event::StageFailed {
            node_id:     "work".to_string(),
            name:        "Work".to_string(),
            index:       1,
            failure:     FailureDetail::new("try again", FailureCategory::TransientInfra),
            will_retry:  true,
            duration_ms: 10,
            actor:       None,
        },
        workflow_event::Event::StageRetrying {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        1,
            attempt:      2,
            max_attempts: 3,
            delay_ms:     100,
        },
    ])
    .await;

    let mut node_outcomes = HashMap::new();
    node_outcomes.insert("setup".to_string(), Outcome::success());
    let mut checkpoint = Checkpoint {
        timestamp: Utc::now(),
        current_node: "setup".to_string(),
        completed_nodes: vec!["setup".to_string()],
        node_retries: HashMap::new(),
        context_values: HashMap::new(),
        node_outcomes,
        next_node_id: Some("work".to_string()),
        git_commit_sha: None,
        loop_failure_signatures: HashMap::new(),
        restart_failure_signatures: HashMap::new(),
        node_visits: HashMap::new(),
    };

    let run_dir = std::env::temp_dir().join(format!("fabro-server-test-{run_id}"));
    std::fs::create_dir_all(&run_dir).unwrap();
    let mut managed = managed_run(
        MINIMAL_DOT.to_string(),
        RunStatus::Running,
        Utc::now(),
        run_dir,
        RunExecutionMode::Start,
    );
    managed.checkpoint = Some(checkpoint.clone());
    state
        .runs
        .lock()
        .expect("runs lock poisoned")
        .insert(run_id, managed);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/stages")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(stage_status(&body, "setup"), "succeeded");
    assert_eq!(stage_status(&body, "work"), "retrying");

    let mut work_outcome = Outcome::success();
    work_outcome.status = StageOutcome::PartiallySucceeded;
    checkpoint.completed_nodes.push("work".to_string());
    checkpoint
        .node_outcomes
        .insert("work".to_string(), work_outcome);
    checkpoint.current_node = "work".to_string();
    checkpoint.next_node_id = Some("exit".to_string());
    state
        .runs
        .lock()
        .expect("runs lock poisoned")
        .get_mut(&run_id)
        .unwrap()
        .checkpoint = Some(checkpoint);

    let run_store = state.store.open_run(&run_id).await.unwrap();
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::StageCompleted {
            node_id: "work".to_string(),
            name: "Work".to_string(),
            index: 1,
            duration_ms: 25,
            status: "partially_succeeded".to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing: None,
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            response: None,
            attempt: 2,
            max_attempts: 3,
        },
    )
    .await
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/stages")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(stage_status(&body, "work"), "partially_succeeded");
}

async fn append_raw_run_event(
    state: &Arc<AppState>,
    run_id: RunId,
    seq_hint: &str,
    ts: &str,
    event: &str,
    properties: serde_json::Value,
    node_id: Option<&str>,
) {
    let run_store = state.store.open_run(&run_id).await.unwrap();
    let payload = fabro_store::EventPayload::new(
        json!({
            "id": format!("evt-{seq_hint}"),
            "ts": ts,
            "run_id": run_id,
            "event": event,
            "node_id": node_id,
            "properties": properties,
        }),
        &run_id,
    )
    .unwrap();
    run_store.append_event(&payload).await.unwrap();
}

fn github_token_settings() -> ServerSettings {
    ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.integrations.github]
strategy = "token"
"#,
    )
    .expect("github token settings fixture should resolve")
}

fn create_github_token_app_state(
    token: Option<&str>,
    github_api_base_url: Option<String>,
) -> Arc<AppState> {
    create_github_token_app_state_with_env_lookup(token, github_api_base_url, |_| None)
}

fn create_github_token_app_state_with_env_lookup(
    token: Option<&str>,
    github_api_base_url: Option<String>,
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
) -> Arc<AppState> {
    let (store, artifact_store) = test_store_bundle();
    let vault_path = test_secret_store_path();
    let server_env_path = vault_path.with_file_name("server.env");
    let config = AppStateConfig {
        resolved_settings: resolved_runtime_settings_for_tests(
            github_token_settings(),
            RunLayer::default(),
        ),
        registry_factory_override: None,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        vault_path,
        server_secrets: load_test_server_secrets(server_env_path, HashMap::new()),
        env_lookup: Arc::new(env_lookup),
        github_api_base_url,
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
    };
    let state = build_app_state(config).expect("test app state should build");
    if let Some(token) = token {
        state
            .vault
            .try_write()
            .expect("test vault should not already be locked")
            .set("GITHUB_TOKEN", token, SecretType::Credential, None)
            .expect("test github token should be writable");
    }
    state
}

/// Build the (state, router, run_id) triple every PR-endpoint test
/// needs. Use this instead of repeating the
/// state/build_router/fixtures::RUN_1 incantation per test.
fn pr_test_app(
    token: Option<&str>,
    github_api_base_url: Option<String>,
) -> (Arc<AppState>, Router, RunId) {
    let state = create_github_token_app_state(token, github_api_base_url);
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    (state, app, fixtures::RUN_1)
}

/// Same as [`pr_test_app`] but creates a fresh minimal run via the
/// HTTP create-run endpoint instead of using fixtures::RUN_1. For
/// tests that exercise endpoints expecting a real on-disk run rather
/// than a synthetic fixture id.
async fn pr_test_app_with_minimal_run(
    token: Option<&str>,
    github_api_base_url: Option<String>,
) -> (Arc<AppState>, Router, String) {
    let state = create_github_token_app_state(token, github_api_base_url);
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT).await;
    (state, app, run_id)
}

/// Same as [`pr_test_app`] but the run is set up as a completed
/// workflow ready for `POST /runs/{id}/pull_request`. The branches
/// and diff are fixed defaults; only the origin URL varies per
/// test (None to test missing-origin rejection, gitlab.com to test
/// non-github rejection, etc.).
async fn pr_test_app_with_completed_run(
    token: Option<&str>,
    github_api_base_url: Option<String>,
    repo_origin_url: Option<&str>,
) -> (Arc<AppState>, Router, RunId) {
    let (state, app, run_id) = pr_test_app(token, github_api_base_url);
    create_completed_run_ready_for_pull_request(
        &state,
        run_id,
        repo_origin_url,
        Some("main"),
        Some("fabro/run/42"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+fn shipped() {}\n",
    )
    .await;
    (state, app, run_id)
}

async fn create_run_with_pull_request_record(
    state: &Arc<AppState>,
    run_id: RunId,
    pr_url: &str,
    pr_number: u64,
    title: &str,
) {
    create_durable_run_with_events(state, run_id, &[
        workflow_event::Event::PullRequestCreated {
            pr_url: pr_url.to_string(),
            pr_number,
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
            base_branch: "main".to_string(),
            head_branch: "feature".to_string(),
            title: title.to_string(),
            draft: false,
        },
    ])
    .await;
}

async fn create_completed_run_ready_for_pull_request(
    state: &Arc<AppState>,
    run_id: RunId,
    repo_origin_url: Option<&str>,
    base_branch: Option<&str>,
    run_branch: Option<&str>,
    final_patch: &str,
) {
    let mut graph = Graph::new("test");
    graph.attrs.insert(
        "goal".to_string(),
        AttrValue::String("Ship the server-side PR".to_string()),
    );
    let git = match (repo_origin_url, base_branch) {
        (Some(origin), Some(branch)) => Some(fabro_types::GitContext {
            origin_url:   origin.to_string(),
            branch:       branch.to_string(),
            sha:          None,
            dirty:        fabro_types::DirtyStatus::Clean,
            push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
        }),
        _ => None,
    };
    let run_spec = RunSpec {
        run_id,
        settings: fabro_types::WorkflowSettings::default(),
        graph,
        workflow_slug: Some("test".to_string()),
        source_directory: Some("/tmp/project".to_string()),
        git: git.clone(),
        labels: HashMap::new(),
        provenance: None,
        manifest_blob: None,
        definition_blob: None,
        fork_source_ref: None,
        in_place: false,
    };

    create_durable_run_with_events(state, run_id, &[
        workflow_event::Event::RunCreated {
            run_id,
            settings: serde_json::to_value(&run_spec.settings).unwrap(),
            graph: serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source: None,
            workflow_config: None,
            labels: run_spec.labels.clone().into_iter().collect(),
            run_dir: run_spec.source_directory.clone().unwrap_or_default(),
            source_directory: run_spec.source_directory.clone(),
            workflow_slug: run_spec.workflow_slug.clone(),
            db_prefix: None,
            provenance: run_spec.provenance.clone(),
            manifest_blob: None,
            git,
            fork_source_ref: None,
            in_place: false,
        },
        workflow_event::Event::WorkflowRunStarted {
            name: "test".to_string(),
            run_id,
            base_branch: base_branch.map(str::to_string),
            base_sha: None,
            run_branch: run_branch.map(str::to_string),
            worktree_dir: None,
            goal: Some("Ship the server-side PR".to_string()),
        },
        workflow_event::Event::WorkflowRunCompleted {
            duration_ms:          1,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          Some(final_patch.to_string()),
            billing:              None,
        },
    ])
    .await;
}

fn test_event_envelope(seq: u32, run_id: RunId, body: EventBody) -> EventEnvelope {
    EventEnvelope {
        seq,
        event: RunEvent {
            id: format!("evt-{seq}"),
            ts: Utc::now(),
            run_id,
            node_id: None,
            node_label: None,
            stage_id: None,
            parallel_group_id: None,
            parallel_branch_id: None,
            session_id: None,
            parent_session_id: None,
            tool_call_id: None,
            actor: None,
            body,
        },
    }
}

#[tokio::test]
async fn test_model_unknown_returns_404() {
    let app = test_app_with();

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/nonexistent-model-xyz/test"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn test_model_alias_returns_canonical_model_id() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/sonnet/test"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["model_id"], "claude-sonnet-4-6");
    assert_eq!(body["status"], "skip");
}

#[tokio::test]
async fn test_model_invalid_mode_returns_400() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/claude-opus-4-6/test?mode=bogus"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn list_models_filters_by_provider() {
    let app = test_app_with();

    let req = Request::builder()
        .method("GET")
        .uri(api("/models?provider=anthropic"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let models = body["data"].as_array().unwrap();
    assert!(!models.is_empty());
    assert!(
        models
            .iter()
            .all(|model| model["provider"] == serde_json::Value::String("anthropic".into()))
    );
}

#[tokio::test]
async fn list_models_filters_by_query_across_aliases() {
    let app = test_app_with();

    let req = Request::builder()
        .method("GET")
        .uri(api("/models?query=codex"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let model_ids = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(model_ids, vec![
        "gpt-5.2-codex".to_string(),
        "gpt-5.3-codex".to_string(),
        "gpt-5.3-codex-spark".to_string()
    ]);
}

#[tokio::test]
async fn list_models_marks_configured_true_when_provider_has_credential_material() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |name| (name == EnvVars::ANTHROPIC_API_KEY).then(|| "test-key".to_string()),
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("GET")
        .uri(api("/models"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let models = body["data"].as_array().unwrap();

    assert!(models.iter().any(|model| model["provider"] != "anthropic"));
    assert!(models.iter().any(|model| model["provider"] == "anthropic"));
    assert!(
        models
            .iter()
            .filter(|model| model["provider"] == "anthropic")
            .all(|model| model["configured"].as_bool() == Some(true))
    );
    assert!(
        models
            .iter()
            .filter(|model| model["provider"] != "anthropic")
            .all(|model| model["configured"].as_bool() == Some(false))
    );
}

#[tokio::test]
async fn list_models_marks_configured_false_when_no_credential_material() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("GET")
        .uri(api("/models"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let models = body["data"].as_array().unwrap();

    assert!(!models.is_empty());
    assert!(
        models
            .iter()
            .all(|model| model["configured"].as_bool() == Some(false))
    );
}

#[tokio::test]
async fn list_models_invalid_provider_returns_400() {
    let app = test_app_with();

    let req = Request::builder()
        .method("GET")
        .uri(api("/models?provider=not-a-provider"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn auth_login_github_redirects_to_github() {
    let source = r#"
_version = 1

[server.auth]
methods = ["github"]

[server.web]
enabled = true
url = "http://localhost:3000"

[server.auth.github]
allowed_usernames = ["octocat"]

[server.integrations.github]
app_id = "123"
client_id = "Iv1.testclient"
slug = "fabro"
"#;
    let app = build_router(
        test_app_state_with_session_key(
            server_settings_from_toml(source),
            manifest_run_defaults_from_toml(source),
            Some("github-redirect-test-key-0123456789"),
        ),
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::Github],
            dev_token:  None,
            jwt_key:    None,
            jwt_issuer: None,
        }),
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/auth/login/github")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let response = checked_response!(response, StatusCode::SEE_OTHER).await;
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(location.starts_with("https://github.com/login/oauth/authorize?"));
}

#[tokio::test]
async fn logout_redirects_to_login_page() {
    let app = test_app_with();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let response = checked_response!(response, StatusCode::SEE_OTHER).await;
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/login")
    );
}

#[tokio::test]
async fn static_favicon_is_served() {
    let app = test_app_with();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/images/favicon.svg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let response = checked_response!(response, StatusCode::OK).await;
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("image/svg+xml")
    );
}

#[tokio::test]
async fn post_runs_starts_run_and_returns_id() {
    let app = test_app_with();

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    assert!(body["id"].is_string());
    assert!(!body["id"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn post_runs_invalid_dot_returns_bad_request() {
    let app = test_app_with();

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body("not a graph"))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_run_status_returns_status() {
    let state = test_app_state();
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    // Give run a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Check status
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["run_id"].as_str().unwrap(), run_id);
    assert_eq!(body["goal"].as_str().unwrap(), "Test");
    assert_eq!(body["title"].as_str().unwrap(), "Test");
    assert!(body["repository"].is_object());
    assert!(!body["repository"]["name"].as_str().unwrap().is_empty());
    assert!(body["created_at"].is_string());
    assert!(body["labels"].is_object());
}

#[tokio::test]
async fn get_run_status_not_found() {
    let app = test_app_with();
    let missing_run_id = fixtures::RUN_64;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{missing_run_id}")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn resolve_run_returns_unique_run_id_prefix_match() {
    let app = test_app_with();
    let run_id = create_run(&app, MINIMAL_DOT).await;
    let selector = &run_id[..8];

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/resolve?selector={selector}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["run_id"], run_id);
}

#[tokio::test]
async fn resolve_run_returns_bad_request_for_ambiguous_prefix() {
    let app = test_app_with();
    let run_id_a = create_run(&app, MINIMAL_DOT).await;
    let run_id_b = create_run(&app, MINIMAL_DOT).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/runs/resolve?selector=0"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json!(response, StatusCode::BAD_REQUEST).await;
    let detail = body["errors"][0]["detail"]
        .as_str()
        .expect("error detail should be present");
    assert!(
        detail.contains(&run_id_a),
        "detail should mention first run: {detail}"
    );
    assert!(
        detail.contains(&run_id_b),
        "detail should mention second run: {detail}"
    );
    assert!(
        detail.contains("created_at="),
        "detail should include creation timestamps: {detail}"
    );
    assert!(
        detail.contains("workflow="),
        "detail should include workflow names: {detail}"
    );
    assert!(
        detail.contains("origin="),
        "detail should include origin URLs: {detail}"
    );
}

#[tokio::test]
async fn resolve_run_prefers_most_recent_exact_workflow_slug_match() {
    let app = test_app_with();
    let older_id = create_run_for_target(
        &app,
        "ship-feature.fabro",
        &named_workflow_dot("ShipFeatureAlpha", "older"),
    )
    .await;
    let newer_id = create_run_for_target(
        &app,
        "ship-feature.fabro",
        &named_workflow_dot("ShipFeatureBeta", "newer"),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/runs/resolve?selector=ship-feature"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["run_id"], newer_id);
    assert_ne!(body["run_id"], older_id);
}

#[tokio::test]
async fn resolve_run_prefers_most_recent_collapsed_workflow_name_match() {
    let app = test_app_with();
    let older_id = create_run_for_target(
        &app,
        "nightly-alpha.fabro",
        &named_workflow_dot("Nightly_Build", "older"),
    )
    .await;
    let newer_id = create_run_for_target(
        &app,
        "nightly-beta.fabro",
        &named_workflow_dot("Nightly_Build", "newer"),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/runs/resolve?selector=nightlybuild"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["run_id"], newer_id);
    assert_ne!(body["run_id"], older_id);
}

#[tokio::test]
async fn resolve_run_returns_not_found_for_unknown_selector() {
    let app = test_app_with();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/runs/resolve?selector=missing-run"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_questions_returns_empty_list() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Start a run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    // Get questions (should be empty for a run without wait.human nodes)
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/questions")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert!(body["data"].is_array());
    assert_eq!(body["meta"]["has_more"], false);
}

#[tokio::test]
async fn submit_answer_not_found_run() {
    let app = test_app_with();
    let missing_run_id = fixtures::RUN_64;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{missing_run_id}/questions/q1/answer")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"value": "yes"})).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn submit_pending_interview_answer_rejects_invalid_answer_shape() {
    let state = test_app_state();
    let pending = LoadedPendingInterview {
        run_id:   fixtures::RUN_1,
        qid:      "q-1".to_string(),
        question: InterviewQuestionRecord {
            id:              "q-1".to_string(),
            text:            "Approve deploy?".to_string(),
            stage:           "gate".to_string(),
            question_type:   QuestionType::MultipleChoice,
            options:         vec![fabro_types::run_event::InterviewOption {
                key:   "approve".to_string(),
                label: "Approve".to_string(),
            }],
            allow_freeform:  false,
            timeout_seconds: None,
            context_display: None,
        },
    };

    let response = submit_pending_interview_answer(
        state.as_ref(),
        &pending,
        AnswerSubmission::system(
            Answer::text("not a valid multiple choice answer"),
            SystemActorKind::Engine,
        ),
    )
    .await
    .unwrap_err();

    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn get_events_not_found() {
    let app = test_app_with();
    let missing_run_id = fixtures::RUN_64;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{missing_run_id}/events")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_run_state_returns_projection() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/state")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert!(body["stages"].is_object());
}

#[tokio::test]
async fn get_run_logs_returns_per_run_log_file() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[workflow_event::Event::RunSubmitted {
        definition_blob: None,
    }])
    .await;
    let log_path = Storage::new(state.server_storage_dir())
        .run_scratch(&run_id)
        .runtime_dir()
        .join("server.log");
    tokio::fs::create_dir_all(log_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&log_path, b"worker log line\nsecond line\n")
        .await
        .unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/logs")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response_bytes!(response, StatusCode::OK).await;

    assert_eq!(content_type.as_deref(), Some("text/plain; charset=utf-8"));
    assert_eq!(&body[..], b"worker log line\nsecond line\n");
}

#[tokio::test]
async fn get_run_logs_returns_not_found_for_missing_run() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(state);
    let missing_run_id = RunId::new();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{missing_run_id}/logs")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_run_logs_returns_not_found_when_log_file_is_missing() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[workflow_event::Event::RunSubmitted {
        definition_blob: None,
    }])
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/logs")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_run_stage_command_log_returns_scratch_slice() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let stage_id = StageId::new("script_node", 1);
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::StageStarted {
            node_id:      "script_node".to_string(),
            name:         "Script".to_string(),
            index:        1,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
        workflow_event::Event::CommandStarted {
            node_id:    "script_node".to_string(),
            script:     "echo hello world".to_string(),
            command:    "echo hello world".to_string(),
            language:   "shell".to_string(),
            timeout_ms: None,
        },
    ])
    .await;
    let run_dir = Storage::new(state.server_storage_dir())
        .run_scratch(&run_id)
        .root()
        .to_path_buf();
    let log_path = command_log_path(&run_dir, &stage_id, CommandOutputStream::Stdout);
    tokio::fs::create_dir_all(log_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&log_path, b"hello world").await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/logs/stdout?offset=6&limit=5"
        )))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let bytes = BASE64_STANDARD
        .decode(body["bytes_base64"].as_str().unwrap())
        .unwrap();

    assert_eq!(body["stream"], "stdout");
    assert_eq!(body["offset"], 6);
    assert_eq!(body["next_offset"], 11);
    assert_eq!(body["total_bytes"], 11);
    assert_eq!(bytes, b"world");
    assert_eq!(body["eof"], false);
    assert_eq!(body["cas_ref"], serde_json::Value::Null);
    assert_eq!(body["live_streaming"], true);
}

#[tokio::test]
async fn get_run_stage_command_log_returns_cas_slice() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let run_store = state.store.create_run(&run_id).await.unwrap();
    let stdout_blob = run_store
        .write_blob(&serde_json::to_vec("hello world").unwrap())
        .await
        .unwrap();
    let stderr_blob = run_store
        .write_blob(&serde_json::to_vec("").unwrap())
        .await
        .unwrap();
    let stdout_ref = format!("blob://sha256/{stdout_blob}");
    let stderr_ref = format!("blob://sha256/{stderr_blob}");
    for event in [
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::StageStarted {
            node_id:      "script_node".to_string(),
            name:         "Script".to_string(),
            index:        1,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
        workflow_event::Event::CommandCompleted {
            node_id:           "script_node".to_string(),
            stdout:            stdout_ref.clone(),
            stderr:            stderr_ref,
            exit_code:         Some(0),
            duration_ms:       5,
            termination:       CommandTermination::Exited,
            stdout_bytes:      11,
            stderr_bytes:      0,
            streams_separated: true,
            live_streaming:    false,
        },
    ] {
        workflow_event::append_event(&run_store, &run_id, &event)
            .await
            .unwrap();
    }

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/script_node@1/logs/stdout?offset=6&limit=5"
        )))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let bytes = BASE64_STANDARD
        .decode(body["bytes_base64"].as_str().unwrap())
        .unwrap();

    assert_eq!(body["stream"], "stdout");
    assert_eq!(body["offset"], 6);
    assert_eq!(body["next_offset"], 11);
    assert_eq!(body["total_bytes"], 11);
    assert_eq!(bytes, b"world");
    assert_eq!(body["eof"], true);
    assert_eq!(body["cas_ref"], stdout_ref);
    assert_eq!(body["live_streaming"], false);
}

#[tokio::test]
async fn get_run_stage_command_log_prefers_scratch_when_cas_ref_exists() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let stage_id = StageId::new("script_node", 1);
    let run_store = state.store.create_run(&run_id).await.unwrap();
    let stdout_blob = run_store
        .write_blob(&serde_json::to_vec("cas log").unwrap())
        .await
        .unwrap();
    let stderr_blob = run_store
        .write_blob(&serde_json::to_vec("").unwrap())
        .await
        .unwrap();
    let stdout_ref = format!("blob://sha256/{stdout_blob}");
    let stderr_ref = format!("blob://sha256/{stderr_blob}");
    for event in [
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::StageStarted {
            node_id:      "script_node".to_string(),
            name:         "Script".to_string(),
            index:        1,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
        workflow_event::Event::CommandCompleted {
            node_id:           "script_node".to_string(),
            stdout:            stdout_ref.clone(),
            stderr:            stderr_ref,
            exit_code:         Some(0),
            duration_ms:       5,
            termination:       CommandTermination::Exited,
            stdout_bytes:      7,
            stderr_bytes:      0,
            streams_separated: true,
            live_streaming:    false,
        },
    ] {
        workflow_event::append_event(&run_store, &run_id, &event)
            .await
            .unwrap();
    }

    let run_dir = Storage::new(state.server_storage_dir())
        .run_scratch(&run_id)
        .root()
        .to_path_buf();
    let log_path = command_log_path(&run_dir, &stage_id, CommandOutputStream::Stdout);
    tokio::fs::create_dir_all(log_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&log_path, b"scratch log").await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/logs/stdout?offset=0&limit=64"
        )))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let bytes = BASE64_STANDARD
        .decode(body["bytes_base64"].as_str().unwrap())
        .unwrap();

    assert_eq!(body["stream"], "stdout");
    assert_eq!(body["offset"], 0);
    assert_eq!(body["next_offset"], 11);
    assert_eq!(body["total_bytes"], 11);
    assert_eq!(bytes, b"scratch log");
    assert_eq!(body["eof"], true);
    assert_eq!(body["cas_ref"], stdout_ref);
    assert_eq!(body["live_streaming"], false);
}

#[tokio::test]
async fn get_run_stage_command_log_returns_not_found_for_missing_stage() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[workflow_event::Event::RunSubmitted {
        definition_blob: None,
    }])
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/stages/missing@1/logs/stdout")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_run_pull_request_returns_live_detail_from_github() {
    let github = MockServer::start();
    let github_mock = github.mock(|when, then| {
        when.method("GET")
            .path("/repos/acme/widgets/pulls/42")
            .header("authorization", "Bearer ghu_test");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                json!({
                    "number": 42,
                    "title": "Fix the bug",
                    "body": "Detailed description",
                    "state": "closed",
                    "draft": false,
                    "merged": true,
                    "merged_at": "2026-04-23T15:45:00Z",
                    "mergeable": false,
                    "additions": 10,
                    "deletions": 3,
                    "changed_files": 2,
                    "html_url": "https://github.com/acme/widgets/pull/42",
                    "user": { "login": "testuser" },
                    "head": { "ref": "feature" },
                    "base": { "ref": "main" },
                    "created_at": "2026-04-23T15:40:00Z",
                    "updated_at": "2026-04-23T15:45:00Z"
                })
                .to_string(),
            );
    });
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), Some(github.base_url()));

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["record"]["number"], 42);
    assert_eq!(body["record"]["owner"], "acme");
    assert_eq!(body["state"], "closed");
    assert_eq!(body["merged"], true);
    assert_eq!(body["head"]["ref"], "feature");
    assert_eq!(body["base"]["ref"], "main");
    github_mock.assert();
}

#[tokio::test]
async fn get_run_pull_request_returns_not_found_when_record_missing() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::NOT_FOUND).await;

    assert_eq!(body["errors"][0]["code"], "no_stored_record");
}

#[tokio::test]
async fn get_run_pull_request_rejects_non_github_record_url() {
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://gitlab.com/acme/widgets/-/merge_requests/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;

    assert_eq!(body["errors"][0]["code"], "unsupported_host");
}

#[tokio::test]
async fn get_run_pull_request_returns_service_unavailable_without_github_credentials() {
    let (state, app, run_id) = pr_test_app(None, None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::SERVICE_UNAVAILABLE).await;

    assert_eq!(body["errors"][0]["code"], "integration_unavailable");
}

#[tokio::test]
async fn get_run_pull_request_returns_bad_gateway_when_github_pr_is_missing() {
    let github = MockServer::start();
    let github_mock = github.mock(|when, then| {
        when.method("GET")
            .path("/repos/acme/widgets/pulls/42")
            .header("authorization", "Bearer ghu_test");
        then.status(404)
            .header("content-type", "application/json")
            .body(json!({ "message": "Not Found" }).to_string());
    });
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), Some(github.base_url()));

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_GATEWAY).await;

    assert_eq!(body["errors"][0]["code"], "github_not_found");
    github_mock.assert();
}

#[tokio::test]
async fn create_run_pull_request_creates_and_persists_record() {
    let github = MockServer::start();
    let create_mock = github.mock(|when, then| {
        when.method("POST")
            .path("/repos/acme/widgets/pulls")
            .header("authorization", "Bearer ghu_test");
        then.status(201)
            .header("content-type", "application/json")
            .body(
                json!({
                    "html_url": "https://github.com/acme/widgets/pull/42",
                    "number": 42,
                    "node_id": "PR_kwDOAA"
                })
                .to_string(),
            );
    });
    let llm = MockServer::start_async().await;
    let response_mock = llm
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/responses")
                .header("authorization", "Bearer openai-key");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(openai_responses_payload("Narrative from mock."));
        })
        .await;
    let openai_base_url = llm.url("/v1");
    let state = create_github_token_app_state_with_env_lookup(
        Some("ghu_test"),
        Some(github.base_url()),
        move |name| match name {
            "OPENAI_BASE_URL" => Some(openai_base_url.clone()),
            _ => None,
        },
    );
    state
        .vault
        .write()
        .await
        .set(
            "openai_codex",
            &serde_json::to_string(&openai_api_key_credential("openai-key")).unwrap(),
            SecretType::Credential,
            None,
        )
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    create_completed_run_ready_for_pull_request(
        &state,
        run_id,
        Some("git@github.com:acme/widgets.git"),
        Some("main"),
        Some("fabro/run/42"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+fn shipped() {}\n",
    )
    .await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "force": false,
                        "model": "gpt-5.4"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["number"], 42);
    assert_eq!(body["owner"], "acme");
    assert_eq!(body["repo"], "widgets");
    assert_eq!(body["html_url"], "https://github.com/acme/widgets/pull/42");

    let state_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/state")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let state_body = response_json!(state_response, StatusCode::OK).await;
    assert_eq!(state_body["pull_request"]["number"], 42);
    assert!(state_body["pull_request"]["title"].as_str().is_some());

    response_mock.assert_async().await;
    create_mock.assert();
}

#[tokio::test]
async fn create_run_pull_request_returns_conflict_when_record_exists() {
    let (state, app, run_id) = pr_test_app(None, None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "force": false, "model": null }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::CONFLICT).await;

    assert_eq!(body["errors"][0]["code"], "pull_request_exists");
    assert!(
        body["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("https://github.com/acme/widgets/pull/42")
    );
}

#[tokio::test]
async fn create_run_pull_request_rejects_missing_repo_origin() {
    let (_state, app, run_id) = pr_test_app_with_completed_run(None, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "force": false,
                        "model": "claude-sonnet-4-6"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;

    assert_eq!(body["errors"][0]["code"], "missing_repo_origin");
}

#[tokio::test]
async fn create_run_pull_request_returns_service_unavailable_without_github_credentials() {
    let (_state, app, run_id) =
        pr_test_app_with_completed_run(None, None, Some("https://github.com/acme/widgets.git"))
            .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "force": false,
                        "model": "claude-sonnet-4-6"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::SERVICE_UNAVAILABLE).await;

    assert_eq!(body["errors"][0]["code"], "integration_unavailable");
}

#[tokio::test]
async fn create_run_pull_request_rejects_non_github_origin_url() {
    let (_state, app, run_id) = pr_test_app_with_completed_run(
        Some("ghu_test"),
        None,
        Some("https://gitlab.com/acme/widgets.git"),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "force": false,
                        "model": "claude-sonnet-4-6"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;

    assert_eq!(body["errors"][0]["code"], "unsupported_host");
}

#[tokio::test]
async fn pull_request_endpoints_use_github_base_url_captured_at_startup() {
    let github = MockServer::start();
    let captured_mock = github.mock(|when, then| {
        when.method("GET")
            .path("/repos/acme/widgets/pulls/42")
            .header("authorization", "Bearer ghu_test");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                json!({
                    "number": 42,
                    "title": "Captured",
                    "body": "",
                    "state": "open",
                    "draft": false,
                    "merged": false,
                    "mergeable": true,
                    "additions": 1,
                    "deletions": 0,
                    "changed_files": 1,
                    "html_url": "https://github.com/acme/widgets/pull/42",
                    "user": { "login": "octocat" },
                    "head": { "ref": "feature" },
                    "base": { "ref": "main" },
                    "created_at": "2026-04-23T12:00:00Z",
                    "updated_at": "2026-04-23T12:00:00Z"
                })
                .to_string(),
            );
    });
    let state = create_github_token_app_state(Some("ghu_test"), Some(github.base_url()));
    assert_eq!(state.github_api_base_url, github.base_url());

    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Captured",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    response_json!(response, StatusCode::OK).await;

    // If the handler read GITHUB_BASE_URL at request time instead of using the
    // value captured at AppState construction, the outbound call would miss
    // this mock — no other server is running at the captured URL, and the
    // process env default points elsewhere.
    captured_mock.assert();
}

#[tokio::test]
async fn merge_run_pull_request_returns_not_found_when_record_missing() {
    let (_state, app, run_id) = pr_test_app_with_minimal_run(Some("ghu_test"), None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/merge")))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "method": "squash" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::NOT_FOUND).await;

    assert_eq!(body["errors"][0]["code"], "no_stored_record");
}

#[tokio::test]
async fn merge_run_pull_request_rejects_invalid_method() {
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/merge")))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "method": "bogus" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn merge_run_pull_request_rejects_non_github_record_url() {
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://gitlab.com/acme/widgets/-/merge_requests/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/merge")))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "method": "squash" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;

    assert_eq!(body["errors"][0]["code"], "unsupported_host");
}

#[tokio::test]
async fn merge_run_pull_request_returns_service_unavailable_without_github_credentials() {
    let (state, app, run_id) = pr_test_app(None, None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/merge")))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "method": "squash" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::SERVICE_UNAVAILABLE).await;

    assert_eq!(body["errors"][0]["code"], "integration_unavailable");
}

#[tokio::test]
async fn close_run_pull_request_returns_not_found_when_record_missing() {
    let (_state, app, run_id) = pr_test_app_with_minimal_run(Some("ghu_test"), None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/close")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::NOT_FOUND).await;

    assert_eq!(body["errors"][0]["code"], "no_stored_record");
}

#[tokio::test]
async fn close_run_pull_request_rejects_non_github_record_url() {
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://gitlab.com/acme/widgets/-/merge_requests/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/close")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;

    assert_eq!(body["errors"][0]["code"], "unsupported_host");
}

#[tokio::test]
async fn close_run_pull_request_returns_service_unavailable_without_github_credentials() {
    let (state, app, run_id) = pr_test_app(None, None);

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/close")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::SERVICE_UNAVAILABLE).await;

    assert_eq!(body["errors"][0]["code"], "integration_unavailable");
}

#[tokio::test]
async fn close_run_pull_request_returns_bad_gateway_when_github_pr_is_missing() {
    let github = MockServer::start();
    let github_mock = github.mock(|when, then| {
        when.method("PATCH")
            .path("/repos/acme/widgets/pulls/42")
            .header("authorization", "Bearer ghu_test");
        then.status(404)
            .header("content-type", "application/json")
            .body(json!({ "message": "Not Found" }).to_string());
    });
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), Some(github.base_url()));

    create_run_with_pull_request_record(
        &state,
        run_id,
        "https://github.com/acme/widgets/pull/42",
        42,
        "Fix the bug",
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/pull_request/close")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_GATEWAY).await;

    assert_eq!(body["errors"][0]["code"], "github_not_found");
    github_mock.assert();
}

#[tokio::test]
async fn get_run_state_exposes_pending_interviews() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "pending-question",
        "2026-04-19T12:00:00Z",
        "interview.started",
        json!({
            "question_id": "q-1",
            "question": "Approve deploy?",
            "stage": "gate",
            "question_type": "multiple_choice",
            "options": [],
            "allow_freeform": false,
            "context_display": null,
            "timeout_seconds": null,
        }),
        Some("gate"),
    )
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/state")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(
        body["pending_interviews"]["q-1"]["question"]["text"].as_str(),
        Some("Approve deploy?")
    );
    assert_eq!(
        body["pending_interviews"]["q-1"]["question"]["stage"].as_str(),
        Some("gate")
    );
}

#[tokio::test]
async fn get_run_state_includes_provenance_from_user_agent() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .header("user-agent", "fabro-cli/1.2.3")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/state")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(
        body["spec"]["provenance"]["server"]["version"],
        FABRO_VERSION
    );
    assert_eq!(
        body["spec"]["provenance"]["client"]["user_agent"],
        "fabro-cli/1.2.3"
    );
    assert_eq!(body["spec"]["provenance"]["client"]["name"], "fabro-cli");
    assert_eq!(body["spec"]["provenance"]["client"]["version"], "1.2.3");
    assert_eq!(body["spec"]["provenance"]["subject"]["kind"], "user");
    assert_eq!(
        body["spec"]["provenance"]["subject"]["auth_method"],
        "dev_token"
    );
    assert_eq!(body["spec"]["provenance"]["subject"]["login"], "dev");
    assert_eq!(
        body["spec"]["provenance"]["subject"]["identity"]["issuer"],
        "fabro:dev"
    );
}

#[tokio::test]
async fn dev_token_web_login_authorizes_cookie_backed_api_requests() {
    const DEV_TOKEN: &str =
        "fabro_dev_abababababababababababababababababababababababababababababababab";

    let state = test_app_state_with_session_key(
        default_test_server_settings(),
        RunLayer::default(),
        Some("server-test-session-key-0123456789"),
    );
    let app = build_router(
        Arc::clone(&state),
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::DevToken],
            dev_token:  Some(DEV_TOKEN.to_string()),
            jwt_key:    Some(
                auth::derive_jwt_key(b"server-test-session-key-0123456789")
                    .expect("test JWT key should derive"),
            ),
            jwt_issuer: Some("https://fabro.example".to_string()),
        }),
    );

    let login_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login/dev-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "token": DEV_TOKEN }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let login_response = checked_response!(login_response, StatusCode::OK).await;
    let session_cookie = login_response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .expect("session cookie should be set")
        .to_string();

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &session_cookie)
                .body(manifest_body(MINIMAL_DOT))
                .unwrap(),
        )
        .await
        .unwrap();
    let create_body = response_json!(create_response, StatusCode::CREATED).await;
    let run_id = create_body["id"].as_str().unwrap();

    let state_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/state")))
                .header(header::COOKIE, &session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let state_body = response_json!(state_response, StatusCode::OK).await;
    assert_eq!(
        state_body["spec"]["provenance"]["subject"]["auth_method"],
        "dev_token"
    );
    assert_eq!(state_body["spec"]["provenance"]["subject"]["login"], "dev");
}

#[tokio::test]
async fn create_run_persists_manifest_and_definition_blobs_without_bundle_file() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let raw_manifest = serde_json::to_string_pretty(&minimal_manifest_json(MINIMAL_DOT)).unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(raw_manifest.clone()))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let run_store = state.store.open_run_reader(&run_id).await.unwrap();
    let events = run_store.list_events().await.unwrap();
    let created = events[0].event.to_value().unwrap();
    let submitted = events[1].event.to_value().unwrap();
    let manifest_blob = created["properties"]["manifest_blob"]
        .as_str()
        .expect("run.created should carry manifest_blob")
        .parse::<RunBlobId>()
        .unwrap();
    let definition_blob = submitted["properties"]["definition_blob"]
        .as_str()
        .expect("run.submitted should carry definition_blob")
        .parse::<RunBlobId>()
        .unwrap();

    let submitted_manifest_bytes = run_store
        .read_blob(&manifest_blob)
        .await
        .unwrap()
        .expect("submitted manifest blob should exist");
    assert_eq!(submitted_manifest_bytes.as_ref(), raw_manifest.as_bytes());

    let accepted_definition_bytes = run_store
        .read_blob(&definition_blob)
        .await
        .unwrap()
        .expect("accepted definition blob should exist");
    let accepted_definition: serde_json::Value =
        serde_json::from_slice(&accepted_definition_bytes).unwrap();
    assert!(
        accepted_definition.get("version").is_none(),
        "accepted run definition should not carry compatibility versioning"
    );
    assert_eq!(accepted_definition["workflow_path"], "workflow.fabro");
    assert!(accepted_definition["workflows"]["workflow.fabro"].is_object());

    created["properties"]["run_dir"]
        .as_str()
        .expect("run.created should include run_dir");
}

#[tokio::test]
async fn list_run_events_returns_paginated_json() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/events?since_seq=1&limit=5")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert!(body["data"].is_array());
    assert!(body["meta"]["has_more"].is_boolean());
}

#[tokio::test]
async fn append_run_event_rejects_run_id_mismatch() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/events")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "id": "evt-test",
                "ts": "2026-03-27T12:00:00Z",
                "run_id": fixtures::RUN_64.to_string(),
                "event": "run.submitted",
                "properties": {}
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn append_run_event_rejects_reserved_archive_event() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT).await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/events")))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "id": "evt-run-archived",
                "ts": "2026-04-19T12:00:00Z",
                "run_id": run_id,
                "event": "run.archived",
                "properties": {
                    "actor": null
                }
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;
    assert!(
        body["errors"][0]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("run.archived is a lifecycle event")),
        "expected lifecycle rejection, got: {body}"
    );
}

#[tokio::test]
async fn get_checkpoint_returns_null_initially() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Start a run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    // Get checkpoint immediately (before run completes, may be null)
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/checkpoint")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    checked_response!(response, StatusCode::OK).await;
}

#[tokio::test]
async fn write_and_read_run_blob_round_trip() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/blobs")))
        .header("content-type", "application/octet-stream")
        .body(Body::from("hello blob"))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let blob_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/blobs/{blob_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let bytes = response_bytes!(response, StatusCode::OK).await;
    assert_eq!(&bytes[..], b"hello blob");
}

#[tokio::test]
async fn stage_artifacts_round_trip() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_run(&app, MINIMAL_DOT).await;
    let stage_id = "code@2";

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/artifacts?filename=src/lib.rs&retry=1"
        )))
        .header("content-type", "application/octet-stream")
        .body(Body::from("fn main() {}"))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NO_CONTENT).await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/stages/{stage_id}/artifacts")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["data"][0]["filename"], "src/lib.rs");
    assert_eq!(body["data"][0]["retry"], 1);
    assert_eq!(body["data"][0]["size"], 12);

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/artifacts/download?filename=src/lib.rs"
        )))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/artifacts/download?filename=src/lib.rs&retry=1"
        )))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let bytes = response_bytes!(response, StatusCode::OK).await;
    assert_eq!(&bytes[..], b"fn main() {}");
}

#[tokio::test]
async fn stage_artifacts_keep_same_filename_per_retry() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_run(&app, MINIMAL_DOT).await;
    let stage_id = "code@2";

    for (retry, body) in [(1, "first"), (2, "second")] {
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!(
                "/runs/{run_id}/stages/{stage_id}/artifacts?filename=logs/output.txt&retry={retry}"
            )))
            .header("content-type", "application/octet-stream")
            .body(Body::from(body))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_status!(response, StatusCode::NO_CONTENT).await;
    }

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/stages/{stage_id}/artifacts")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["data"][0]["filename"], "logs/output.txt");
    assert_eq!(body["data"][0]["retry"], 1);
    assert_eq!(body["data"][1]["filename"], "logs/output.txt");
    assert_eq!(body["data"][1]["retry"], 2);

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/artifacts/download?filename=logs/output.txt&retry=2"
        )))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let bytes = response_bytes!(response, StatusCode::OK).await;
    assert_eq!(&bytes[..], b"second");
}

#[tokio::test]
async fn create_run_persists_run_spec() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let run_state = state
        .store
        .open_run_reader(&run_id)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();

    assert!(run_state.spec.is_some());
}

#[tokio::test]
async fn stage_artifact_upload_rejects_invalid_filename() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_run(&app, MINIMAL_DOT).await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/stages/code@2/artifacts?filename=../escape.txt&retry=1"
        )))
        .header("content-type", "application/octet-stream")
        .body(Body::from("nope"))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn worker_token_accepts_run_scoped_routes_and_falls_back_to_user_jwt() {
    let (state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let other_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let other_worker_token = issue_test_worker_token(&other_run_id);
    let blob_id = state
        .store
        .open_run(&run_id)
        .await
        .unwrap()
        .write_blob(b"preloaded blob")
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/state"),
            &worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let append_body = serde_json::to_vec(&serde_json::json!({
        "id": "evt-run-notice",
        "ts": "2026-04-23T12:00:00Z",
        "event": "run.notice",
        "run_id": run_id.to_string(),
        "properties": {
            "level": "info",
            "code": "worker",
            "message": "hello"
        }
    }))
    .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(api(&format!("/runs/{run_id}/events")))
                .header(header::AUTHORIZATION, format!("Bearer {worker_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(append_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/events"),
            &worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/runs/{run_id}/blobs"),
            &worker_token,
            Body::from("worker blob"),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/blobs/{blob_id}"),
            &worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/state"),
            &user_jwt,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/state"),
            &other_worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn worker_token_controls_stage_artifact_route() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let other_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let mismatched_worker_token = issue_test_worker_token(&other_run_id);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(api(&format!(
                    "/runs/{run_id}/stages/code@2/artifacts?filename=artifact.txt&retry=1"
                )))
                .header(header::AUTHORIZATION, format!("Bearer {worker_token}"))
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from("artifact"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::NO_CONTENT).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(api(&format!(
                    "/runs/{run_id}/stages/code@2/artifacts?filename=artifact.txt&retry=1"
                )))
                .header(header::AUTHORIZATION, format!("Bearer {user_jwt}"))
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from("artifact"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::NO_CONTENT).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(api(&format!(
                    "/runs/{run_id}/stages/code@2/artifacts?filename=artifact.txt&retry=1"
                )))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {mismatched_worker_token}"),
                )
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from("artifact"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::FORBIDDEN).await;

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(api(&format!(
                    "/runs/{run_id}/stages/code@2/artifacts?filename=artifact.txt&retry=1"
                )))
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from("artifact"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn worker_token_controls_command_log_route() {
    let (state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let other_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let mismatched_worker_token = issue_test_worker_token(&other_run_id);
    let run_store = state.store.open_run(&run_id).await.unwrap();
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::CommandStarted {
            node_id:    "code".to_string(),
            script:     "echo hello".to_string(),
            command:    "echo hello".to_string(),
            language:   "shell".to_string(),
            timeout_ms: None,
        },
    )
    .await
    .unwrap();

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/stages/code@1/logs/stdout"),
            &worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/stages/code@1/logs/stdout"),
            &user_jwt,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/stages/code@1/logs/stdout"),
            &mismatched_worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::FORBIDDEN).await;

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(api(&format!("/runs/{run_id}/stages/code@1/logs/stdout")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn worker_token_is_rejected_on_user_only_routes() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let blob_id = RunBlobId::new(b"blob");
    let user_only_routes = vec![
        (Method::GET, "/runs".to_string()),
        (Method::POST, "/runs".to_string()),
        (Method::GET, "/runs/resolve".to_string()),
        (Method::POST, "/preflight".to_string()),
        (Method::POST, "/validate".to_string()),
        (Method::POST, "/graph/render".to_string()),
        (Method::GET, "/attach".to_string()),
        (Method::GET, "/boards/runs".to_string()),
        (Method::GET, format!("/runs/{run_id}")),
        (Method::DELETE, format!("/runs/{run_id}")),
        (Method::GET, format!("/runs/{run_id}/questions")),
        (Method::POST, format!("/runs/{run_id}/questions/q-1/answer")),
        (Method::GET, format!("/runs/{run_id}/attach")),
        (Method::GET, format!("/runs/{run_id}/checkpoint")),
        (Method::POST, format!("/runs/{run_id}/cancel")),
        (Method::POST, format!("/runs/{run_id}/start")),
        (Method::POST, format!("/runs/{run_id}/pause")),
        (Method::POST, format!("/runs/{run_id}/unpause")),
        (Method::POST, format!("/runs/{run_id}/archive")),
        (Method::POST, format!("/runs/{run_id}/unarchive")),
        (Method::GET, format!("/runs/{run_id}/graph")),
        (Method::GET, format!("/runs/{run_id}/graph/source")),
        (Method::GET, format!("/runs/{run_id}/stages")),
        (Method::GET, format!("/runs/{run_id}/artifacts")),
        (Method::GET, format!("/runs/{run_id}/files")),
        (
            Method::GET,
            format!("/runs/{run_id}/stages/code@2/artifacts"),
        ),
        (
            Method::GET,
            format!("/runs/{run_id}/stages/code@2/artifacts/download"),
        ),
        (Method::GET, format!("/runs/{run_id}/billing")),
        (Method::GET, format!("/runs/{run_id}/settings")),
        (Method::POST, format!("/runs/{run_id}/preview")),
        (Method::POST, format!("/runs/{run_id}/ssh")),
        (Method::GET, format!("/runs/{run_id}/sandbox/files")),
        (Method::GET, format!("/runs/{run_id}/sandbox/file")),
        (Method::PUT, format!("/runs/{run_id}/sandbox/file")),
    ];

    for (method, path) in user_only_routes {
        let response = app
            .clone()
            .oneshot(bearer_request(
                method.clone(),
                &path,
                &worker_token,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ),
            "{method} {path} unexpectedly accepted worker token with status {}",
            response.status()
        );
    }

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run_id}/blobs/{blob_id}"),
            &worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stage_artifacts_multipart_round_trip() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_run(&app, MINIMAL_DOT).await;
    let stage_id = "code@2";
    let source_bytes = b"fn main() {}\n";
    let log_bytes = b"build ok\n";
    let manifest = serde_json::json!({
        "entries": [
            {
                "part": "file1",
                "path": "src/lib.rs",
                "sha256": hex::encode(Sha256::digest(source_bytes)),
                "expected_bytes": source_bytes.len(),
                "content_type": "text/plain"
            },
            {
                "part": "file2",
                "path": "logs/output.txt",
                "sha256": hex::encode(Sha256::digest(log_bytes)),
                "expected_bytes": log_bytes.len(),
                "content_type": "text/plain"
            }
        ]
    });
    let boundary = "fabro-test-boundary";

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/artifacts?retry=1"
        )))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_body(boundary, &manifest, &[
            ("file1", "src/lib.rs", source_bytes),
            ("file2", "logs/output.txt", log_bytes),
        ]))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NO_CONTENT).await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/stages/{stage_id}/artifacts")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["data"][0]["filename"], "logs/output.txt");
    assert_eq!(body["data"][0]["retry"], 1);
    assert_eq!(body["data"][0]["size"], log_bytes.len());
    assert_eq!(body["data"][1]["filename"], "src/lib.rs");
    assert_eq!(body["data"][1]["retry"], 1);
    assert_eq!(body["data"][1]["size"], source_bytes.len());

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/artifacts/download?filename=logs/output.txt&retry=1"
        )))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let bytes = response_bytes!(response, StatusCode::OK).await;
    assert_eq!(&bytes[..], log_bytes);
}

#[tokio::test]
async fn stage_artifacts_multipart_requires_manifest_first() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_run(&app, MINIMAL_DOT).await;
    let boundary = "fabro-test-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file1\"; filename=\"src/lib.rs\"\r\n\r\nfn main() {{}}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"manifest\"\r\nContent-Type: application/json\r\n\r\n{{\"entries\":[{{\"part\":\"file1\",\"path\":\"src/lib.rs\"}}]}}\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/stages/code@2/artifacts?retry=1"
        )))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn create_run_returns_submitted() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    assert_eq!(body["status"]["kind"], "submitted");
}

#[tokio::test]
async fn start_run_transitions_to_queued() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Create a run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    // Start it
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["status"]["kind"], "queued");

    let status = state
        .store
        .open_run_reader(&run_id.parse::<RunId>().unwrap())
        .await
        .unwrap()
        .state()
        .await
        .unwrap()
        .status
        .unwrap();
    assert_eq!(status, RunStatus::Queued);
}

#[tokio::test]
async fn start_run_conflict_when_not_submitted() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Create a run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    // Start it (transitions to queued)
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap();

    // Start it again — should 409
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::CONFLICT).await;
}

#[tokio::test]
async fn cancel_run_succeeds() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_and_start_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();

    // Cancel it
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    // Could be OK (cancelled) or CONFLICT (already completed)
    let status = response.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::CONFLICT,
        "unexpected status: {status}"
    );
}

#[tokio::test]
async fn cancel_nonexistent_run_returns_not_found() {
    let app = test_app_with();
    let missing_run_id = fixtures::RUN_64;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{missing_run_id}/cancel")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_graph_returns_svg() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Start a run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "version": 1,
                "cwd": "/tmp",
                "target": {
                    "identifier": "workflow.fabro",
                    "path": "workflow.fabro",
                },
                "workflows": {
                    "workflow.fabro": {
                        "source": MINIMAL_DOT,
                        "files": {},
                    },
                },
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    // Request graph SVG
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/graph")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    let response = checked_response!(response, StatusCode::OK).await;

    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header should be present")
        .to_str()
        .unwrap();
    assert_eq!(content_type, "image/svg+xml");

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let svg = String::from_utf8_lossy(&bytes);
    assert!(
        svg.contains("<?xml") || svg.contains("<svg"),
        "expected SVG content, got: {}",
        &svg[..svg.len().min(200)]
    );
}

#[tokio::test]
async fn get_graph_source_returns_dot() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "version": 1,
                "cwd": "/tmp",
                "target": {
                    "identifier": "workflow.fabro",
                    "path": "workflow.fabro",
                },
                "workflows": {
                    "workflow.fabro": {
                        "source": MINIMAL_DOT,
                        "files": {},
                    },
                },
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/graph/source")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let response = checked_response!(response, StatusCode::OK).await;

    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header should be present")
        .to_str()
        .unwrap();
    assert_eq!(content_type, "text/vnd.graphviz");

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let dot = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(dot, MINIMAL_DOT);
}

#[tokio::test]
async fn render_graph_from_manifest_returns_svg() {
    let app = test_app_with();

    let req = Request::builder()
        .method("POST")
        .uri(api("/graph/render"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "manifest": {
                    "version": 1,
                    "cwd": "/tmp",
                    "target": {
                        "identifier": "workflow.fabro",
                        "path": "workflow.fabro",
                    },
                    "workflows": {
                        "workflow.fabro": {
                            "source": MINIMAL_DOT,
                            "files": {},
                        },
                    },
                },
                "format": "svg",
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    let response = checked_response!(response, StatusCode::OK).await;
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .expect("content-type header should be present")
            .to_str()
            .unwrap(),
        "image/svg+xml"
    );

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let svg = String::from_utf8_lossy(&bytes);
    assert!(
        svg.contains("<?xml") || svg.contains("<svg"),
        "expected SVG content, got: {}",
        &svg[..svg.len().min(200)]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn render_graph_bytes_returns_bad_request_for_render_error_protocol() {
    let (_dir, script_path) = write_test_executable(
        "#!/bin/sh\ncat >/dev/null\nprintf 'RENDER_ERROR:failed to parse DOT source'\nexit 0\n",
    );

    let response =
        render_graph_bytes_with_exe_override("not valid dot {{{", Some(&script_path)).await;

    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[cfg(unix)]
fn write_test_executable(script: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir should exist");
    let path = dir.path().join("fake-fabro");
    std::fs::write(&path, script).expect("script should be written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("script should be executable");
    (dir, path)
}

#[cfg(unix)]
async fn render_graph_with_override(dot_source: &str, exe_path: &Path) -> Response {
    render_graph_bytes_with_exe_override(dot_source, Some(exe_path)).await
}

#[cfg(unix)]
#[tokio::test]
async fn render_dot_subprocess_returns_child_crashed_for_nonzero_exit() {
    let (_dir, script_path) = write_test_executable("#!/bin/sh\nexit 1\n");

    let result = render_dot_subprocess("digraph { a -> b }", Some(&script_path)).await;

    assert!(matches!(
        result,
        Err(RenderSubprocessError::ChildCrashed(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn render_graph_bytes_returns_internal_server_error_for_child_crash() {
    let (_dir, script_path) = write_test_executable("#!/bin/sh\nexit 1\n");

    let response = render_graph_with_override("digraph { a -> b }", &script_path).await;

    assert_status!(response, StatusCode::INTERNAL_SERVER_ERROR).await;
}

#[cfg(unix)]
#[tokio::test]
async fn render_dot_subprocess_returns_protocol_violation_for_garbage_stdout() {
    let (_dir, script_path) =
        write_test_executable("#!/bin/sh\ncat >/dev/null\nprintf 'garbage'\nexit 0\n");

    let result = render_dot_subprocess("digraph { a -> b }", Some(&script_path)).await;

    assert!(matches!(
        result,
        Err(RenderSubprocessError::ProtocolViolation(_))
    ));
}

#[tokio::test]
async fn get_graph_not_found() {
    let app = test_app_with();
    let missing_run_id = fixtures::RUN_64;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{missing_run_id}/graph")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn list_runs_returns_started_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // List should be empty initially
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
    assert_eq!(body["meta"]["has_more"].as_bool(), Some(false));

    // Start a run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    // List should now contain one run
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let items = body["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["run_id"].as_str().unwrap(), run_id.to_string());
    assert!(items[0]["goal"].is_string());
    assert!(items[0]["title"].is_string());
    assert!(items[0]["repository"]["name"].is_string());
    assert!(items[0]["created_at"].is_string());
    assert!(items[0]["status"].is_object());
    assert!(items[0]["labels"].is_object());
    assert!(items[0]["pending_control"].is_null());
    assert!(items[0]["total_usd_micros"].is_null());
}

#[tokio::test]
async fn archive_and_unarchive_updates_listing_visibility() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::WorkflowRunCompleted {
            duration_ms:          1000,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            billing:              None,
        },
    ])
    .await;

    let archive_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/archive")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let archive_body = response_json!(archive_response, StatusCode::OK).await;
    assert_eq!(archive_body["status"]["kind"], "archived");
    assert_eq!(archive_body["status"]["prior"]["kind"], "succeeded");
    assert_eq!(archive_body["status"]["prior"]["reason"], "completed");

    let hidden_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/runs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let hidden_body = response_json!(hidden_response, StatusCode::OK).await;
    assert!(
        !hidden_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["run_id"].as_str() == Some(&run_id.to_string())),
        "archived run should be hidden from default listing"
    );

    let visible_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/runs?include_archived=true"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let visible_body = response_json!(visible_response, StatusCode::OK).await;
    let archived_item = visible_body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["run_id"].as_str() == Some(&run_id.to_string()))
        .expect("archived run should appear when include_archived=true");
    assert_eq!(archived_item["status"]["kind"], "archived");
    assert_eq!(archived_item["status"]["prior"]["kind"], "succeeded");
    assert_eq!(archived_item["status"]["prior"]["reason"], "completed");

    let unarchive_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/unarchive")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let unarchive_body = response_json!(unarchive_response, StatusCode::OK).await;
    assert_eq!(unarchive_body["status"]["kind"], "succeeded");
    assert_eq!(unarchive_body["status"]["reason"], "completed");

    let restored_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/runs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let restored_body = response_json!(restored_response, StatusCode::OK).await;
    let restored_item = restored_body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["run_id"].as_str() == Some(&run_id.to_string()))
        .expect("unarchived run should reappear in default listing");
    assert_eq!(restored_item["status"]["kind"], "succeeded");
    assert_eq!(restored_item["status"]["reason"], "completed");
}

#[tokio::test]
async fn archive_unknown_run_returns_not_found() {
    let app = test_app_with();
    let run_id = fixtures::RUN_64;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{run_id}/archive")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn delete_run_removes_durable_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}?force=true")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NO_CONTENT).await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn delete_active_run_requires_force() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CONFLICT).await;
    let short_run_id = &run_id[..12.min(run_id.len())];
    let expected = format!(
        "cannot remove active run {short_run_id} (status: submitted, use force=true or --force to force)"
    );
    assert_eq!(
        body["errors"][0]["detail"].as_str(),
        Some(expected.as_str())
    );

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::OK).await;
}

#[tokio::test]
async fn delete_active_run_force_succeeds() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap();

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}?force=true")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NO_CONTENT).await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_aggregate_billing_returns_zeros_initially() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("GET")
        .uri(api("/billing"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["totals"]["runs"].as_i64().unwrap(), 0);
    assert_eq!(body["totals"]["input_tokens"].as_i64().unwrap(), 0);
    assert_eq!(body["totals"]["output_tokens"].as_i64().unwrap(), 0);
    assert_eq!(body["totals"]["runtime_secs"].as_f64().unwrap(), 0.0);
    assert!(body["totals"]["total_usd_micros"].is_null());
    assert!(body["by_model"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn post_runs_returns_submitted_status() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    // Check status is submitted (no start, no scheduler running)
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    assert_eq!(body["status"]["kind"], "submitted");
}

#[tokio::test]
async fn start_run_persists_full_settings_snapshot() {
    let source = r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[run.execution]
mode = "dry_run"

[run.model]
provider = "anthropic"
name = "claude-sonnet-4-5"

[run.sandbox]
provider = "local"

[[run.hooks]]
name = "snapshot-hook"
event = "run_start"
command = ["echo", "snapshot"]
blocking = false
timeout = "1s"
sandbox = false

[run.git.author]
name = "Snapshot Bot"
email = "snapshot@example.com"

[server.integrations.github]
app_id = "12345"

[server.web]
url = "http://example.test"

[server.api]
url = "http://api.example.test"

[server.logging]
level = "debug"
"#;
    let state = test_app_state_with_options(
        server_settings_from_toml(source),
        manifest_run_defaults_from_toml(source),
        5,
    );
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let _run_dir = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        runs.get(&run_id)
            .and_then(|run| run.run_dir.clone())
            .expect("run_dir should be recorded")
    };
    let run_spec = state
        .store
        .open_run_reader(&run_id)
        .await
        .unwrap()
        .state()
        .await
        .unwrap()
        .spec
        .expect("run spec should exist");
    let resolved_run = &run_spec.settings.run;

    // Verify a sampling of the persisted v2 settings, including inherited
    // run execution mode from server settings.
    assert_eq!(
        match &resolved_run.goal {
            Some(fabro_types::settings::run::RunGoal::Inline(value)) => Some(value.as_source()),
            _ => None,
        }
        .as_deref(),
        Some("Test"),
        "goal should be persisted from the manifest"
    );
    assert!(
        resolved_run.execution.mode == fabro_types::settings::run::RunMode::DryRun,
        "run execution mode should inherit from server settings"
    );
    assert_eq!(
        resolved_run
            .model
            .name
            .as_ref()
            .map(fabro_types::settings::InterpString::as_source)
            .as_deref(),
        Some("claude-sonnet-4-5"),
    );

    // Server-operational fields (auth, integrations, etc.) deliberately
    // do not flow into the run's persisted settings — they live on the
    // server and are read via AppState::server_settings().
    let settings_json = serde_json::to_value(&run_spec.settings).unwrap();
    assert!(settings_json.pointer("/server").is_none());
}

#[tokio::test]
async fn cancel_queued_run_succeeds() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_and_start_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();

    // Cancel it
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::OK).await;

    // Verify status is cancelled
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    assert_eq!(body["status"]["kind"], "failed");
    assert_eq!(body["status"]["reason"], "cancelled");

    // Cancelled runs appear on the board in the "failed" column
    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id_str = run_id.to_string();
    let board_item = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["run_id"].as_str() == Some(run_id_str.as_str()));
    assert!(
        board_item.is_some(),
        "cancelled run should appear on the board"
    );
    assert_eq!(
        board_item.unwrap()["status"]["kind"].as_str(),
        Some("failed"),
        "cancelled run should preserve the failed lifecycle status"
    );
    assert_eq!(board_item.unwrap()["column"].as_str(), Some("failed"));

    let run_store = state.store.open_run_reader(&run_id).await.unwrap();
    let status = run_store.state().await.unwrap().status.unwrap();
    assert_eq!(status, RunStatus::Failed {
        reason: FailureReason::Cancelled,
    });
}

#[tokio::test]
async fn cancel_run_overwrites_pending_pause_request() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.worker_pid = Some(u32::MAX);
    }
    append_control_request(state.as_ref(), run_id, RunControlAction::Pause, None)
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["pending_control"].as_str(), Some("cancel"));

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(summary.pending_control, Some(RunControlAction::Cancel));
}

#[tokio::test]
async fn pause_run_rejects_when_control_is_already_pending() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.worker_pid = Some(u32::MAX);
    }
    append_control_request(state.as_ref(), run_id, RunControlAction::Cancel, None)
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/pause")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::CONFLICT).await;

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(summary.pending_control, Some(RunControlAction::Cancel));
}

#[tokio::test]
async fn pause_run_sets_pending_control_on_board_response() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.worker_pid = Some(u32::MAX);
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/pause")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["status"]["kind"], "running");
    assert_eq!(body["pending_control"].as_str(), Some("pause"));

    // Verify pending_control via /runs/{id} (board no longer includes this field)
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    assert_eq!(body["pending_control"].as_str(), Some("pause"));

    // Verify the run appears on the board (store has Submitted status →
    // "initializing" column)
    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let item = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["run_id"].as_str() == Some(run_id_str.as_str()))
        .expect("board item should exist");
    assert!(item["status"].is_object());
    assert_eq!(item["column"].as_str(), Some("initializing"));
    assert_eq!(item["pending_control"].as_str(), Some("pause"));
}

#[tokio::test]
async fn pause_run_immediately_pauses_blocked_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    append_raw_run_event(
        &state,
        run_id,
        "pause-starting",
        "2026-04-19T11:59:58Z",
        "run.starting",
        json!({}),
        None,
    )
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "pause-running",
        "2026-04-19T11:59:59Z",
        "run.running",
        json!({}),
        None,
    )
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "pause-blocked",
        "2026-04-19T12:00:00Z",
        "run.blocked",
        json!({ "blocked_reason": "human_input_required" }),
        None,
    )
    .await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        };
        managed_run.worker_pid = Some(u32::MAX);
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/pause")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["status"]["kind"], "paused");
    assert_eq!(body["status"]["prior_block"], "human_input_required");
    assert_eq!(body["pending_control"], serde_json::Value::Null);

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(summary.status, RunStatus::Paused {
        prior_block: Some(BlockedReason::HumanInputRequired),
    });
    assert_eq!(summary.pending_control, None);
}

#[tokio::test]
async fn unpause_run_sets_pending_control() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Paused { prior_block: None };
        managed_run.worker_pid = Some(u32::MAX);
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/unpause")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["status"]["kind"], "paused");
    assert!(body["status"]["prior_block"].is_null());
    assert_eq!(body["pending_control"].as_str(), Some("unpause"));

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(summary.pending_control, Some(RunControlAction::Unpause));
}

#[tokio::test]
async fn unpause_run_returns_blocked_when_human_gate_is_still_unresolved() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    append_raw_run_event(
        &state,
        run_id,
        "paused-blocked-starting",
        "2026-04-19T11:59:58Z",
        "run.starting",
        json!({}),
        None,
    )
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "paused-blocked-running",
        "2026-04-19T11:59:59Z",
        "run.running",
        json!({}),
        None,
    )
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "paused-blocked-paused",
        "2026-04-19T12:00:00Z",
        "run.paused",
        json!({}),
        None,
    )
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "paused-blocked-status",
        "2026-04-19T12:00:01Z",
        "run.blocked",
        json!({ "blocked_reason": "human_input_required" }),
        None,
    )
    .await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Paused {
            prior_block: Some(BlockedReason::HumanInputRequired),
        };
        managed_run.worker_pid = Some(u32::MAX);
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/unpause")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["status"]["kind"], "blocked");
    assert_eq!(body["status"]["blocked_reason"], "human_input_required");
    assert_eq!(body["pending_control"], serde_json::Value::Null);

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(summary.status, RunStatus::Blocked {
        blocked_reason: BlockedReason::HumanInputRequired,
    });
    assert_eq!(summary.pending_control, None);
}

#[tokio::test]
async fn startup_reconciliation_marks_inflight_runs_terminal() {
    let state = test_app_state();

    create_durable_run_with_events(&state, fixtures::RUN_1, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
    ])
    .await;
    create_durable_run_with_events(&state, fixtures::RUN_2, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;
    create_durable_run_with_events(&state, fixtures::RUN_3, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::RunPaused,
        workflow_event::Event::RunCancelRequested { actor: None },
    ])
    .await;

    let reconciled = reconcile_incomplete_runs_on_startup(&state).await.unwrap();
    assert_eq!(reconciled, 2);

    let run_1 = state
        .store
        .open_run_reader(&fixtures::RUN_1)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(run_1.status.unwrap(), RunStatus::Submitted);

    let run_2 = state
        .store
        .open_run_reader(&fixtures::RUN_2)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    let run_2_status = run_2.status.unwrap();
    assert_eq!(run_2_status, RunStatus::Failed {
        reason: FailureReason::Terminated,
    });

    let run_3 = state
        .store
        .open_run_reader(&fixtures::RUN_3)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    let run_3_status = run_3.status.unwrap();
    assert_eq!(run_3_status, RunStatus::Failed {
        reason: FailureReason::Cancelled,
    });
    assert_eq!(run_3.pending_control, None);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_active_workers_terminates_process_groups() {
    let state = test_app_state();
    let run_id = fixtures::RUN_4;

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    let temp_dir = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new("sh");
    child
        .arg("-c")
        .arg("trap '' TERM; while :; do sleep 1; done")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    fabro_proc::pre_exec_setpgid(child.as_std_mut());
    let mut child = child.spawn().unwrap();
    let worker_pid = child.id().expect("worker pid should be available");

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let mut run = managed_run(
            String::new(),
            RunStatus::Running,
            chrono::Utc::now(),
            temp_dir.path().join(run_id.to_string()),
            RunExecutionMode::Start,
        );
        run.worker_pid = Some(worker_pid);
        run.worker_pgid = Some(worker_pid);
        runs.insert(run_id, run);
    }

    let terminated = shutdown_active_workers_with_grace(
        &state,
        Duration::from_millis(50),
        Duration::from_millis(10),
    )
    .await
    .unwrap();
    assert_eq!(terminated, 1);

    let exit_status = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .expect("worker should exit after shutdown")
        .expect("wait should succeed");
    assert!(!exit_status.success());
    assert!(!fabro_proc::process_group_alive(worker_pid));

    let run_state = state
        .store
        .open_run_reader(&run_id)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    let run_status = run_state.status.unwrap();
    assert_eq!(run_status, RunStatus::Failed {
        reason: FailureReason::Terminated,
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_startup_persists_cancelled_reason() {
    let source = r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[[run.prepare.steps]]
script = "sleep 5"

[run.prepare]
timeout = "30s"

[run.sandbox]
provider = "local"
"#;
    let state = test_app_state_with_settings_and_registry_factory(
        server_settings_from_toml(source),
        manifest_run_defaults_from_toml(source),
        |interviewer| fabro_workflow::handler::default_registry(interviewer, || None),
    );
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    let runner = tokio::spawn(
        execute_run(Arc::clone(&state), run_id)
            .instrument(tracing::info_span!("run", id = %run_id)),
    );
    let mut live_status_before_cancel = None;
    for _ in 0..50 {
        live_status_before_cancel = {
            let runs = state.runs.lock().expect("runs lock poisoned");
            runs.get(&run_id).map(|run| run.status)
        };
        if matches!(
            live_status_before_cancel,
            Some(
                RunStatus::Queued
                    | RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. }
            )
        ) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        matches!(
            live_status_before_cancel,
            Some(
                RunStatus::Queued
                    | RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. }
            )
        ),
        "run should become cancellable before finishing, saw {live_status_before_cancel:?}"
    );

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let response_status = response.status();
    let response_body = body_json(response.into_body()).await;
    assert_eq!(
        response_status,
        StatusCode::OK,
        "unexpected cancel response body: {response_body}; live status before cancel: {live_status_before_cancel:?}"
    );

    runner.await.unwrap();

    let runs = state.runs.lock().expect("runs lock poisoned");
    let managed_run = runs.get(&run_id).expect("run should exist");
    assert_eq!(managed_run.status, RunStatus::Failed {
        reason: FailureReason::Cancelled,
    });
    drop(runs);

    let run_store = state.store.open_run_reader(&run_id).await.unwrap();

    let mut status_record = None;
    for _ in 0..50 {
        if let Some(record) = run_store.state().await.unwrap().status {
            if record
                == (RunStatus::Failed {
                    reason: FailureReason::Cancelled,
                })
            {
                status_record = Some(record);
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let status_record = status_record.expect("status record should be persisted");
    assert_eq!(status_record, RunStatus::Failed {
        reason: FailureReason::Cancelled,
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::disallowed_methods,
    reason = "This test intentionally blocks inside a sync registry factory to simulate slow startup before cancellation."
)]
async fn cancel_before_run_transitions_to_running_returns_empty_attach_stream() {
    let state = test_app_state_with_registry_factory(|interviewer| {
        std::thread::sleep(std::time::Duration::from_millis(200));
        fabro_workflow::handler::default_registry(interviewer, || None)
    });
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id_str = create_and_start_run(&app, MINIMAL_DOT).await;
    let run_id = run_id_str.parse::<RunId>().unwrap();

    let runner = tokio::spawn(
        execute_run(Arc::clone(&state), run_id)
            .instrument(tracing::info_span!("run", id = %run_id)),
    );
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::OK).await;

    runner.await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_bytes!(response, StatusCode::OK).await;
    assert!(body.is_empty(), "expected an empty attach stream");
}

#[tokio::test]
async fn queue_position_reported_for_queued_runs() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Create and start two runs (no scheduler, both stay queued)
    let first_run_id = create_and_start_run(&app, MINIMAL_DOT).await;
    let second_run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    // Queued runs are excluded from the board, so verify queue positions
    // via the in-memory state directly.
    let runs = state.runs.lock().expect("runs lock poisoned");
    let positions = compute_queue_positions(&runs);
    let first_id = first_run_id.parse::<RunId>().unwrap();
    let second_id = second_run_id.parse::<RunId>().unwrap();
    assert_eq!(positions.get(&first_id).copied(), Some(1));
    assert_eq!(positions.get(&second_id).copied(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrency_limit_respected() {
    let state = test_app_state_with_options(default_test_server_settings(), RunLayer::default(), 1);
    let app = test_app_with_scheduler(Arc::clone(&state));

    // Create and start two runs with max_concurrent_runs=1
    create_and_start_run(&app, MINIMAL_DOT).await;
    create_and_start_run(&app, MINIMAL_DOT).await;

    // Give scheduler time to pick up the first run
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // The board only shows runs with a visible board column. With
    // max_concurrent_runs=1, at most one run should land in the live
    // "running" column.
    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let items = body["data"].as_array().unwrap();
    let active_count = items
        .iter()
        .filter(|item| item["column"].as_str() == Some("running"))
        .count();
    assert!(
        active_count <= 1,
        "expected at most 1 active run on the board, got {active_count}"
    );
}

#[tokio::test]
async fn submit_answer_to_queued_run_returns_conflict() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(manifest_body(MINIMAL_DOT))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().to_string();

    // Try to submit an answer to a queued run
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/questions/q1/answer")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"value": "yes"})).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::CONFLICT).await;
}

#[tokio::test]
async fn create_completion_missing_messages_returns_422() {
    let app = test_app_with();

    let req = Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::UNPROCESSABLE_ENTITY).await;
}

#[tokio::test]
async fn demo_boards_runs_returns_run_list_items() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);
    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    assert!(!data.is_empty(), "demo should return runs");
    let first = &data[0];
    assert!(first["run_id"].is_string());
    assert!(first["goal"].is_string());
    assert!(first["repository"].is_object());
    assert!(first["title"].is_string());
    assert!(first["status"].is_object());
    assert!(first["column"].is_string());
    assert!(first["workflow_slug"].is_string() || first["workflow_slug"].is_null());
    assert!(first["labels"].is_object());
    assert!(first["created_at"].is_string());
}

#[tokio::test]
async fn demo_get_run_returns_run_summary_shape() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);
    let run_id = RunId::with_timestamp(
        "2026-03-06T14:30:00Z"
            .parse()
            .expect("demo timestamp should parse"),
        1,
    );
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    // Should have RunSummary fields, not RunStatusResponse fields
    assert!(body["run_id"].is_string(), "should have run_id field");
    assert!(body["goal"].is_string(), "should have goal field");
    assert!(
        body["workflow_slug"].is_string(),
        "should have workflow_slug field"
    );
    // Should NOT have RunStatusResponse-only fields
    assert!(
        body["queue_position"].is_null(),
        "should not have queue_position"
    );
}

#[tokio::test]
async fn demo_get_run_returns_404_for_unknown_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs/nonexistent-run-id"))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn boards_runs_returns_run_list_items_with_board_columns() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    // Set run to running so it appears on the board
    {
        let id = run_id.parse::<RunId>().unwrap();
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&id).expect("run should exist");
        managed_run.status = RunStatus::Running;
    }

    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    let item = data
        .iter()
        .find(|i| i["run_id"].as_str() == Some(&run_id))
        .expect("run should be in board");
    // Should have canonical run summary fields plus board-specific column
    assert!(item["goal"].is_string());
    assert!(item["title"].is_string());
    assert!(item["repository"].is_object());
    assert!(item["workflow_slug"].is_string() || item["workflow_slug"].is_null());
    assert!(item["workflow_name"].is_string() || item["workflow_name"].is_null());
    assert!(item["labels"].is_object());
    assert!(item["status"].is_object());
    assert!(item["column"].is_string());
    assert!(item["created_at"].is_string());
    assert!(item["pending_control"].is_null());
    assert!(item["total_usd_micros"].is_null());
}

#[tokio::test]
async fn boards_runs_excludes_removing_status() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;

    // A run in Removing status should not appear on the board
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::RunRemoving,
    ])
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    let found = data
        .iter()
        .any(|i| i["run_id"].as_str() == Some(&run_id.to_string()));
    assert!(!found, "removing run should not appear on the board");
}

#[tokio::test]
async fn get_run_exposes_canonical_operator_statuses() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let succeeded_id = fixtures::RUN_1;
    let removing_id = fixtures::RUN_2;
    let blocked_id = fixtures::RUN_3;

    create_durable_run_with_events(&state, succeeded_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::WorkflowRunCompleted {
            duration_ms:          1000,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            billing:              None,
        },
    ])
    .await;

    create_durable_run_with_events(&state, removing_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::RunRemoving,
    ])
    .await;
    create_durable_run_with_events(&state, blocked_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;
    append_raw_run_event(
        &state,
        blocked_id,
        "status-blocked",
        "2026-04-19T12:00:00Z",
        "run.blocked",
        json!({ "blocked_reason": "human_input_required" }),
        None,
    )
    .await;

    for (run_id, expected_status) in [
        (succeeded_id, "succeeded"),
        (removing_id, "removing"),
        (blocked_id, "blocked"),
    ] {
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = response_json!(response, StatusCode::OK).await;
        assert_eq!(body["status"]["kind"].as_str(), Some(expected_status));
    }
}

#[tokio::test]
async fn boards_runs_maps_statuses_to_columns() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let paused_id = fixtures::RUN_1;
    let succeeded_id = fixtures::RUN_2;
    let blocked_id = fixtures::RUN_3;

    create_durable_run_with_events(&state, paused_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::RunPaused,
    ])
    .await;
    create_durable_run_with_events(&state, succeeded_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::WorkflowRunCompleted {
            duration_ms:          1000,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            billing:              None,
        },
    ])
    .await;
    create_durable_run_with_events(&state, blocked_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;
    append_raw_run_event(
        &state,
        blocked_id,
        "blocked-question-1",
        "2026-04-19T12:00:00Z",
        "interview.started",
        json!({
            "question_id": "q-older",
            "question": "Older unresolved question?",
            "stage": "gate",
            "question_type": "multiple_choice",
            "options": [],
            "allow_freeform": false,
            "context_display": null,
            "timeout_seconds": null,
        }),
        Some("gate"),
    )
    .await;
    append_raw_run_event(
        &state,
        blocked_id,
        "blocked-question-2",
        "2026-04-19T12:00:01Z",
        "interview.started",
        json!({
            "question_id": "q-newer",
            "question": "Newer unresolved question?",
            "stage": "gate",
            "question_type": "multiple_choice",
            "options": [],
            "allow_freeform": false,
            "context_display": null,
            "timeout_seconds": null,
        }),
        Some("gate"),
    )
    .await;
    append_raw_run_event(
        &state,
        blocked_id,
        "blocked-status",
        "2026-04-19T12:00:02Z",
        "run.blocked",
        json!({ "blocked_reason": "human_input_required" }),
        None,
    )
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let data = body["data"].as_array().expect("data should be array");

    let paused_item = data
        .iter()
        .find(|i| i["run_id"].as_str() == Some(&paused_id.to_string()))
        .expect("paused run should be on board");
    assert_eq!(paused_item["status"]["kind"].as_str().unwrap(), "paused");
    assert!(paused_item["status"]["prior_block"].is_null());
    assert_eq!(paused_item["column"].as_str().unwrap(), "running");

    let succeeded_item = data
        .iter()
        .find(|i| i["run_id"].as_str() == Some(&succeeded_id.to_string()))
        .expect("succeeded run should be on board");
    assert_eq!(
        succeeded_item["status"]["kind"].as_str().unwrap(),
        "succeeded"
    );
    assert_eq!(
        succeeded_item["status"]["reason"].as_str().unwrap(),
        "completed"
    );
    assert_eq!(succeeded_item["column"].as_str().unwrap(), "succeeded");

    let blocked_item = data
        .iter()
        .find(|i| i["run_id"].as_str() == Some(&blocked_id.to_string()))
        .expect("blocked run should be on board");
    assert_eq!(blocked_item["status"]["kind"].as_str().unwrap(), "blocked");
    assert_eq!(
        blocked_item["status"]["blocked_reason"].as_str().unwrap(),
        "human_input_required"
    );
    assert_eq!(blocked_item["column"].as_str().unwrap(), "blocked");
    assert_eq!(
        blocked_item["question"]["text"].as_str(),
        Some("Older unresolved question?")
    );

    // Verify columns are included in the response
    let columns = body["columns"].as_array().expect("columns should be array");
    assert!(!columns.is_empty());
    assert!(columns.iter().any(|c| c["id"].as_str() == Some("running")));
    assert!(columns.iter().any(|c| c["id"].as_str() == Some("blocked")));
    assert!(
        columns
            .iter()
            .any(|c| c["id"].as_str() == Some("succeeded"))
    );
}

#[tokio::test]
async fn boards_runs_includes_live_board_metadata_from_run_state() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_and_start_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let run_store = state.store.open_run(&run_id).await.unwrap();
    for event in [
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::SandboxInitialized {
            provider:          "local".to_string(),
            working_directory: "/sandbox/workdir".to_string(),
            identifier:        Some("sb-test".to_string()),
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
        },
        workflow_event::Event::PullRequestCreated {
            pr_url:      "https://github.com/acme/repo/pull/42".to_string(),
            pr_number:   42,
            owner:       "acme".to_string(),
            repo:        "repo".to_string(),
            base_branch: "main".to_string(),
            head_branch: "fabro/run".to_string(),
            title:       "Fix board metadata".to_string(),
            draft:       false,
        },
        workflow_event::Event::InterviewStarted {
            question_id:     "q-1".to_string(),
            question:        "Ship it?".to_string(),
            stage:           "review".to_string(),
            question_type:   "yes_no".to_string(),
            options:         vec![],
            allow_freeform:  false,
            timeout_seconds: None,
            context_display: None,
        },
    ] {
        workflow_event::append_event(&run_store, &run_id, &event)
            .await
            .unwrap();
    }

    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    let item = data
        .iter()
        .find(|i| i["run_id"].as_str() == Some(&run_id.to_string()))
        .expect("run should be in board");

    assert_eq!(item["pull_request"]["number"].as_u64(), Some(42));
    assert_eq!(item["sandbox"]["id"].as_str(), Some("sb-test"));
    assert_eq!(
        item["sandbox"]["working_directory"].as_str(),
        Some("/sandbox/workdir")
    );
    assert_eq!(item["question"]["text"].as_str(), Some("Ship it?"));
}

#[tokio::test]
async fn boards_runs_page_limit_preserves_metadata_for_paged_items() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let first_run_id = create_and_start_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let second_run_id = create_and_start_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();

    for (run_id, sandbox_id) in [(first_run_id, "sb-first"), (second_run_id, "sb-second")] {
        let run_store = state.store.open_run(&run_id).await.unwrap();
        for event in [
            workflow_event::Event::RunStarting,
            workflow_event::Event::RunRunning,
            workflow_event::Event::SandboxInitialized {
                provider:          "local".to_string(),
                working_directory: "/sandbox/workdir".to_string(),
                identifier:        Some(sandbox_id.to_string()),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
            },
        ] {
            workflow_event::append_event(&run_store, &run_id, &event)
                .await
                .unwrap();
        }
    }

    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs?page[limit]=1"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["meta"]["has_more"].as_bool(), Some(true));

    let data = body["data"].as_array().expect("data should be array");
    assert_eq!(data.len(), 1);

    let item = &data[0];
    let sandbox_id = item["sandbox"]["id"]
        .as_str()
        .expect("paged item should still include sandbox metadata");
    assert!(matches!(sandbox_id, "sb-first" | "sb-second"));
}

#[tokio::test]
async fn filtered_global_events_streams_only_matching_run_ids() {
    let run_one = fixtures::RUN_1;
    let run_two = fixtures::RUN_2;
    let (event_tx, _) = broadcast::channel(8);

    let stream = filtered_global_events(event_tx.subscribe(), Some(HashSet::from([run_one])));

    event_tx
        .send(test_event_envelope(
            1,
            run_two,
            EventBody::RunQueued(fabro_types::run_event::RunStatusEffectProps::default()),
        ))
        .unwrap();
    event_tx
        .send(test_event_envelope(
            2,
            run_one,
            EventBody::RunQueued(fabro_types::run_event::RunStatusEffectProps::default()),
        ))
        .unwrap();
    drop(event_tx);

    let events = stream.collect::<Vec<_>>().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 2);
    assert_eq!(events[0].event.run_id, run_one);
}

#[test]
fn validate_github_slug_accepts_real_names() {
    assert!(super::validate_github_slug("owner", "anthropic", 39).is_ok());
    assert!(super::validate_github_slug("repo", "claude-code", 100).is_ok());
    assert!(super::validate_github_slug("repo", "repo.name_1", 100).is_ok());
}

#[test]
fn validate_github_slug_rejects_path_traversal_and_separators() {
    for bad in ["", "..", "foo/bar", "foo%2Fbar", "foo\\bar", "foo?x", "a b"] {
        assert!(
            super::validate_github_slug("owner", bad, 39).is_err(),
            "expected rejection for {bad:?}"
        );
    }
}

#[test]
fn validate_github_slug_rejects_overlong() {
    let long = "a".repeat(40);
    assert!(super::validate_github_slug("owner", &long, 39).is_err());
}
