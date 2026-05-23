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
use fabro_config::ServerSettingsBuilder;
use fabro_config::bind::Bind;
use fabro_interview::{
    AnswerValue, ControlInterviewer, Interviewer, Question, WorkerControlMessage,
};
use fabro_llm::types::{Message as LlmMessage, Request as LlmRequest};
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{Catalog, ModelRef, ProviderId, ReasoningEffort, Speed};
use fabro_types::settings::ServerAuthMethod;
use fabro_types::{
    AgentBackend, AttrValue, AuthMethod, CommandTermination, FailureCategory, FailureDetail, Graph,
    InterviewQuestionRecord, Node, Outcome, QuestionType, RunBlobId, RunId, RunSpec,
    SandboxProvider, StageContextWindowBreakdownItem, StageContextWindowCategory,
    StageContextWindowCountMethod, StageContextWindowProjection, StageContextWindowStaleness,
    StageContextWindowWarning, StageModelUsage, StageTiming, SuccessReason, SystemActorKind,
    WorkflowSettings, fixtures,
};
use fabro_util::check_report::CheckStatus;
use httpmock::Method::{GET, POST};
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
        LlmCatalogSettings::default(),
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

fn state_test_catalog() -> Arc<Catalog> {
    Arc::new(Catalog::from_builtin().expect("default catalog should build"))
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

fn run_json_id(run: &serde_json::Value) -> Option<&str> {
    run["id"].as_str().or_else(|| run["run_id"].as_str())
}

fn run_json_status(run: &serde_json::Value) -> &serde_json::Value {
    &run["lifecycle"]["status"]
}

fn run_json_pending_control(run: &serde_json::Value) -> &serde_json::Value {
    &run["lifecycle"]["pending_control"]
}

fn run_json_archived(run: &serde_json::Value) -> bool {
    run["lifecycle"]["archived"].as_bool().unwrap_or(false)
}

async fn mock_daytona_auth_probe(server: &MockServer) -> httpmock::Mock<'_> {
    server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/sandbox/paginated")
                .query_param("page", "1")
                .query_param("limit", "1");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "items": [],
                    "total": 0,
                    "page": 1,
                    "totalPages": 0
                }));
        })
        .await
}

async fn mock_daytona_current_key<'a>(
    server: &'a MockServer,
    permissions: Vec<&'static str>,
) -> httpmock::Mock<'a> {
    server
        .mock_async(move |when, then| {
            when.method(GET)
                .path("/api-keys/current")
                .header("authorization", "Bearer dtn_test");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "name": "delete-only",
                    "value": "dtn_****",
                    "createdAt": "2026-05-01T00:00:00Z",
                    "permissions": permissions,
                    "lastUsedAt": null,
                    "expiresAt": null,
                    "userId": "user_123"
                }));
        })
        .await
}

fn openai_oauth_credential() -> fabro_auth::OAuthCredential {
    fabro_auth::OAuthCredential {
        tokens:     fabro_auth::OAuthTokens {
            access_token:  "access".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at:    Utc::now() + ChronoDuration::hours(1),
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
    }
}

fn openai_oauth_credential_json() -> String {
    serde_json::to_string(&openai_oauth_credential()).unwrap()
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
    crate::worker_token::issue_worker_token(&keys, run_id).expect("worker token should issue")
}

fn issue_test_run_tools_worker_token(run_id: &RunId) -> String {
    let keys = WorkerTokenKeys::from_master_secret(TEST_SESSION_SECRET.as_bytes())
        .expect("worker keys should derive");
    crate::worker_token::issue_worker_token_with_scopes(
        &keys,
        run_id,
        crate::worker_token::WorkerScopeSet::run_worker_with_agent_run_tools(),
    )
    .expect("worker token should issue")
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

fn json_bearer_request(
    method: Method,
    path: &str,
    bearer: &str,
    body: &serde_json::Value,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
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

[run.environment]
id = "missing"
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
fn system_sandbox_provider_uses_manifest_defaults() {
    let source = r#"
_version = 1

[run.environment]
id = "daytona"
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

[run.environment]
id = "missing"
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
    use fabro_types::settings::run::EnvironmentProvider;
    assert!(EnvironmentProvider::Docker.is_clone_based());
    assert!(EnvironmentProvider::Daytona.is_clone_based());
    assert!(!EnvironmentProvider::Local.is_clone_based());
}

#[tokio::test]
async fn create_secret_stores_file_secret_outside_token_lookups() {
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
    assert_eq!(
        vault.get_entry("/tmp/test.pem").unwrap().secret_type,
        SecretType::File
    );
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
async fn create_secret_stores_valid_oauth_entries() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "OPENAI_CODEX",
                "value": openai_oauth_credential_json(),
                "type": "oauth"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::OK).await;
    let listed = state.vault.read().await.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "OPENAI_CODEX");
    assert_eq!(listed[0].secret_type, SecretType::Oauth);
    assert!(state.vault.read().await.get("OPENAI_CODEX").is_some());
}

#[tokio::test]
async fn create_secret_rejects_under_scoped_daytona_api_key_and_leaves_vault_unchanged() {
    let server = MockServer::start_async().await;
    let auth = mock_daytona_auth_probe(&server).await;
    let current_key = mock_daytona_current_key(&server, vec![
        "delete:snapshots",
        "delete:sandboxes",
        "delete:volumes",
    ])
    .await;
    let base_url = server.base_url();
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        fabro_config::RunLayer::default(),
        5,
        move |name| match name {
            EnvVars::DAYTONA_API_URL => Some(base_url.clone()),
            _ => None,
        },
    );
    state
        .vault
        .write()
        .await
        .set(
            EnvVars::DAYTONA_API_KEY,
            "existing",
            SecretType::Token,
            None,
        )
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": EnvVars::DAYTONA_API_KEY,
                "value": "dtn_test",
                "type": "token"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;

    assert_eq!(
        body["errors"][0]["detail"],
        "API key 'delete-only' is missing required Daytona scopes: \
         write:snapshots, write:sandboxes. Regenerate the key with all \
         snapshot and sandbox scopes."
    );
    assert_eq!(
        state.vault.read().await.get(EnvVars::DAYTONA_API_KEY),
        Some("existing")
    );
    auth.assert_async().await;
    current_key.assert_async().await;
}

#[tokio::test]
async fn diagnostics_reports_under_scoped_daytona_api_key() {
    let server = MockServer::start_async().await;
    let auth = mock_daytona_auth_probe(&server).await;
    let current_key = mock_daytona_current_key(&server, vec![
        "delete:snapshots",
        "delete:sandboxes",
        "delete:volumes",
    ])
    .await;
    let base_url = server.base_url();
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        fabro_config::RunLayer::default(),
        5,
        move |name| match name {
            EnvVars::DAYTONA_API_URL => Some(base_url.clone()),
            _ => None,
        },
    );
    state
        .vault
        .write()
        .await
        .set(
            EnvVars::DAYTONA_API_KEY,
            "dtn_test",
            SecretType::Token,
            None,
        )
        .unwrap();

    let report = crate::diagnostics::run_all(&state).await;
    let sandbox = report
        .sections
        .iter()
        .flat_map(|section| &section.checks)
        .find(|check| check.name == "Sandbox")
        .expect("sandbox check should be present");

    assert_eq!(sandbox.status, CheckStatus::Error);
    assert_eq!(
        sandbox.summary,
        "Daytona API key is missing required scopes"
    );
    assert_eq!(
        sandbox.details[0].text,
        "missing: write:snapshots, write:sandboxes"
    );
    assert_eq!(
        sandbox.remediation.as_deref(),
        Some(
            "Regenerate the Daytona API key with scopes: write:snapshots, \
             delete:snapshots, write:sandboxes, delete:sandboxes, then \
             `fabro secret set DAYTONA_API_KEY`."
        )
    );
    auth.assert_async().await;
    current_key.assert_async().await;
}

#[tokio::test]
async fn resolve_llm_client_reads_openai_token_from_vault() {
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
            "OPENAI_API_KEY",
            "vault-openai-key",
            SecretType::Token,
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
    async fn resolve(
        &self,
        catalog: &fabro_model::Catalog,
    ) -> anyhow::Result<fabro_auth::ResolvedCredentials> {
        let _ = catalog;
        Err(anyhow::Error::new(std::io::Error::other("credential leaf"))
            .context("credential source context"))
    }

    async fn configured_providers(
        &self,
        catalog: &fabro_model::Catalog,
    ) -> Vec<fabro_model::ProviderId> {
        let _ = catalog;
        Vec::new()
    }
}

#[tokio::test]
async fn resolve_llm_client_from_source_preserves_credential_source_chain() {
    let catalog = state_test_catalog();
    let Err(err) = resolve_llm_client_from_source(&FailingCredentialSource, catalog).await else {
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
async fn llm_source_configured_providers_reads_openai_token_from_vault() {
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
            "OPENAI_API_KEY",
            "vault-openai-key",
            SecretType::Token,
            None,
        )
        .unwrap();

    let catalog = state.catalog();
    assert_eq!(
        state
            .llm_source
            .configured_providers(catalog.as_ref())
            .await,
        vec![ProviderId::openai()]
    );
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
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .env_lookup(|name| match name {
            "OPENAI_ORG_ID" => Some("env-org".to_string()),
            _ => None,
        })
        .provider_base_url("openai", server.url("/v1"))
        .build();
    state
        .vault
        .write()
        .await
        .set(
            "OPENAI_API_KEY",
            "vault-openai-key",
            SecretType::Token,
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
async fn list_secrets_includes_oauth_metadata() {
    let state = test_app_state();
    {
        let mut vault = state.vault.write().await;
        vault
            .set(
                "OPENAI_CODEX",
                &openai_oauth_credential_json(),
                SecretType::Oauth,
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
        .find(|entry| entry["name"] == "OPENAI_CODEX")
        .expect("oauth metadata should be listed");
    assert_eq!(entry["type"], "oauth");
    assert_eq!(entry["description"], "saved auth");
    assert!(entry.get("updated_at").is_some());
    assert!(entry.get("value").is_none());
}

#[tokio::test]
async fn create_secret_rejects_invalid_oauth_json() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "OPENAI_CODEX",
                "value": "{not-json",
                "type": "oauth"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn create_secret_rejects_invalid_oauth_name() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": "1OPENAI",
                "value": openai_oauth_credential_json(),
                "type": "oauth"
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
fn worker_command_default_token_omits_agent_run_tools_scope() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state(storage_dir.path(), &["dev-token"], Some(TEST_DEV_TOKEN));
    let run_id = RunId::new();

    let cmd = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Start,
        storage_dir.path(),
        false,
    )
    .unwrap();

    assert_worker_command_passes_token_only_by_env(&cmd);
    let claims = worker_token_claims(&cmd, state.as_ref());

    assert_eq!(claims.run_id, run_id.to_string());
    assert_eq!(claims.scope.split_whitespace().collect::<Vec<_>>(), vec![
        "run:worker"
    ]);
}

#[cfg(unix)]
#[test]
fn worker_command_opt_in_token_includes_agent_run_tools_scope() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state(storage_dir.path(), &["dev-token"], Some(TEST_DEV_TOKEN));
    let run_id = RunId::new();

    let cmd = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Start,
        storage_dir.path(),
        true,
    )
    .unwrap();

    assert_worker_command_passes_token_only_by_env(&cmd);
    let claims = worker_token_claims(&cmd, state.as_ref());

    assert_eq!(claims.run_id, run_id.to_string());
    assert_eq!(claims.scope.split_whitespace().collect::<Vec<_>>(), vec![
        "run:worker",
        "agent:run_tools"
    ]);
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
        false,
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
        false,
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
        false,
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
        false,
    )
    .unwrap();

    assert_eq!(
        command_env_value(&cmd, EnvVars::FABRO_LOG_DESTINATION),
        EnvOverride::Set("stdout".to_string())
    );
}

#[cfg(unix)]
#[test]
fn worker_command_sets_fabro_config_to_active_absolute_config_path() {
    let storage_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let active_config_path = config_dir.path().join("settings.toml");
    let state = worker_command_test_state_with_active_config_path(
        storage_dir.path(),
        &["dev-token"],
        Some(TEST_DEV_TOKEN),
        active_config_path.clone(),
    );
    let run_id = RunId::new();

    let cmd = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Start,
        storage_dir.path(),
        false,
    )
    .unwrap();

    assert!(active_config_path.is_absolute());
    assert_eq!(
        command_env_value(&cmd, EnvVars::FABRO_CONFIG),
        EnvOverride::Set(active_config_path.display().to_string())
    );
    let worker_args = cmd
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        !worker_args.iter().any(|arg| arg == "--config"),
        "__run-worker argument contract should not grow hidden config args: {worker_args:?}"
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
        false,
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
        false,
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
            LlmCatalogSettings::default(),
        ),
        registry_factory_override: None,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        vault_path,
        server_secrets: ServerSecrets::load(server_env_path, HashMap::new()).unwrap(),
        env_lookup: default_env_lookup(),
        github_api_base_url: None,
        active_config_path: tempfile::tempdir().unwrap().path().join("settings.toml"),
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }) else {
        panic!("build_app_state should require SESSION_SECRET")
    };

    assert!(err.to_string().contains(
        "Fabro server refuses to start: auth is configured but SESSION_SECRET is not set."
    ));
}

#[test]
fn build_app_state_migrates_legacy_vault_file_on_boot() {
    let vault_path = test_secret_store_path();
    let timestamp = "2026-05-18T12:00:00Z";
    let legacy_api_key = json!({
        "provider": "anthropic",
        "type": "api_key",
        "key": "sk-ant-legacy",
    });
    let legacy_oauth = json!({
        "provider": "openai",
        "type": "codex_oauth",
        "tokens": {
            "access_token": "codex-access",
            "refresh_token": "codex-refresh",
            "expires_at": "2026-05-18T13:00:00Z",
        },
        "config": {
            "auth_url": "https://auth.openai.com",
            "token_url": "https://auth.openai.com/oauth/token",
            "client_id": "client",
            "scopes": ["openid", "offline_access"],
            "redirect_uri": "https://auth.openai.com/deviceauth/callback",
            "use_pkce": false,
        },
        "account_id": "acct_legacy",
    });
    let legacy_vault = json!({
        "anthropic": {
            "value": legacy_api_key.to_string(),
            "type": "credential",
            "created_at": timestamp,
            "updated_at": timestamp,
        },
        "openai_codex": {
            "value": legacy_oauth.to_string(),
            "type": "credential",
            "created_at": timestamp,
            "updated_at": timestamp,
        },
        "GITHUB_TOKEN": {
            "value": "ghp_legacy",
            "type": "environment",
            "created_at": timestamp,
            "updated_at": timestamp,
        },
        "/tmp/github.pem": {
            "value": "/tmp/github.pem",
            "type": "file",
            "created_at": timestamp,
            "updated_at": timestamp,
        },
    });
    std::fs::write(
        &vault_path,
        serde_json::to_vec_pretty(&legacy_vault).unwrap(),
    )
    .expect("legacy vault should be writable");

    let state = build_test_app_state_with_vault_path(&vault_path)
        .expect("legacy vault should not prevent server boot");

    let vault = state
        .vault
        .try_read()
        .expect("test vault should not be locked");
    let api_key_entry = vault
        .get_entry("ANTHROPIC_API_KEY")
        .expect("legacy provider credential should be migrated to token name");
    assert_eq!(api_key_entry.secret_type, SecretType::Token);
    assert_eq!(api_key_entry.value, "sk-ant-legacy");
    assert!(vault.get_entry("anthropic").is_none());

    let oauth_entry = vault
        .get_entry("OPENAI_CODEX")
        .expect("legacy Codex credential should be migrated to canonical OAuth name");
    assert_eq!(oauth_entry.secret_type, SecretType::Oauth);
    let oauth: fabro_auth::OAuthCredential =
        serde_json::from_str(&oauth_entry.value).expect("migrated OAuth JSON should parse");
    assert_eq!(oauth.tokens.access_token, "codex-access");
    assert_eq!(oauth.account_id.as_deref(), Some("acct_legacy"));
    assert!(vault.get_entry("openai_codex").is_none());

    assert_eq!(
        vault.get_entry("GITHUB_TOKEN").unwrap().secret_type,
        SecretType::Token
    );
    assert_eq!(
        vault.get_entry("/tmp/github.pem").unwrap().secret_type,
        SecretType::File
    );
}

fn build_test_app_state_with_vault_path(vault_path: &Path) -> anyhow::Result<Arc<AppState>> {
    let (store, artifact_store) = test_store_bundle();
    build_app_state(AppStateConfig {
        resolved_settings: resolved_runtime_settings_for_tests(
            default_test_server_settings(),
            RunLayer::default(),
            LlmCatalogSettings::default(),
        ),
        registry_factory_override: None,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        vault_path: vault_path.to_path_buf(),
        server_secrets: load_test_server_secrets(
            vault_path.with_file_name("server.env"),
            HashMap::new(),
        ),
        env_lookup: default_env_lookup(),
        github_api_base_url: None,
        active_config_path: tempfile::tempdir().unwrap().path().join("settings.toml"),
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
        shutdown: tokio_util::sync::CancellationToken::new(),
    })
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
    worker_command_test_state_inner(
        storage_dir,
        methods,
        dev_token,
        extra_config,
        extra_server_secrets,
        env_lookup,
        None,
    )
}

fn worker_command_test_state_with_active_config_path(
    storage_dir: &Path,
    methods: &[&str],
    dev_token: Option<&str>,
    active_config_path: PathBuf,
) -> Arc<AppState> {
    worker_command_test_state_inner(
        storage_dir,
        methods,
        dev_token,
        "",
        &[],
        |_| None,
        Some(active_config_path),
    )
}

fn worker_command_test_state_inner(
    storage_dir: &Path,
    methods: &[&str],
    dev_token: Option<&str>,
    extra_config: &str,
    extra_server_secrets: &[(&str, &str)],
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    active_config_path: Option<PathBuf>,
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
    let mut builder = TestAppStateBuilder::new()
        .runtime_settings(
            server_settings_from_toml(&source),
            manifest_run_defaults_from_toml(&source),
        )
        .max_concurrent_runs(5)
        .env_lookup(env_lookup)
        .server_secret_env(server_secret_env);
    if let Some(active_config_path) = active_config_path {
        builder = builder.active_config_path(active_config_path);
    }
    builder.build()
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

#[cfg(unix)]
fn assert_worker_command_passes_token_only_by_env(cmd: &Command) {
    assert!(matches!(
        command_env_value(cmd, EnvVars::FABRO_WORKER_TOKEN),
        EnvOverride::Set(_)
    ));
    assert_eq!(
        command_env_value(cmd, EnvVars::FABRO_DEV_TOKEN),
        EnvOverride::Unchanged
    );
    let args = cmd
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    assert!(!args.iter().any(|arg| arg == "--artifact-upload-token"));
    assert!(!args.iter().any(|arg| arg == "--worker-token"));
}

#[cfg(unix)]
fn worker_token_claims(cmd: &Command, state: &AppState) -> crate::worker_token::WorkerTokenClaims {
    let EnvOverride::Set(token) = command_env_value(cmd, EnvVars::FABRO_WORKER_TOKEN) else {
        panic!("worker token env should be set");
    };

    jsonwebtoken::decode::<crate::worker_token::WorkerTokenClaims>(
        &token,
        state.worker_token_keys().decoding_key(),
        state.worker_token_keys().validation(),
    )
    .expect("worker token should decode")
    .claims
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
async fn subprocess_answer_transport_steer_enqueues_plain_steer_message() {
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let transport = RunAnswerTransport::Subprocess { control_tx };
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };

    transport
        .steer("try again".to_string(), actor.clone())
        .await
        .unwrap();

    assert_eq!(
        control_rx.recv().await,
        Some(WorkerControlEnvelope::steer("try again", actor))
    );
}

#[tokio::test]
async fn subprocess_answer_transport_interrupt_enqueues_interrupt_message() {
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let transport = RunAnswerTransport::Subprocess { control_tx };
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };

    transport.interrupt(actor.clone()).await.unwrap();

    assert_eq!(
        control_rx.recv().await,
        Some(WorkerControlEnvelope::interrupt(actor))
    );
}

#[tokio::test]
async fn subprocess_answer_transport_interrupt_then_steer_enqueues_single_combined_message() {
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let transport = RunAnswerTransport::Subprocess { control_tx };
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };

    transport
        .interrupt_then_steer("try again".to_string(), actor.clone())
        .await
        .unwrap();

    assert_eq!(
        control_rx.recv().await,
        Some(WorkerControlEnvelope::interrupt_then_steer(
            "try again",
            actor
        ))
    );
}

#[tokio::test]
async fn subprocess_answer_transport_pair_commands_enqueue_control_messages() {
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(3);
    let transport = RunAnswerTransport::Subprocess { control_tx };
    let run_id = fixtures::RUN_1;
    let pair_id = "01HZX6M29F1CD5YYMHT1F5D7WQ".parse().unwrap();
    let message_id = "01HZX6M4D7Y1QW0Q0P6V8Z4DR5".parse().unwrap();
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };
    let target = PairTarget {
        stage_id:   StageId::new("agent", 1),
        node_label: "Agent".to_string(),
    };

    transport
        .start_pair(run_id, pair_id, target.clone(), actor.clone())
        .await
        .unwrap();
    transport
        .send_pair_message(
            pair_id,
            message_id,
            "inspect this".to_string(),
            Some("client-1".to_string()),
            actor.clone(),
        )
        .await
        .unwrap();
    transport.end_pair(pair_id, actor.clone()).await.unwrap();

    assert_eq!(
        control_rx.recv().await,
        Some(WorkerControlEnvelope::start_pair(
            run_id,
            pair_id,
            target,
            actor.clone()
        ))
    );
    assert_eq!(
        control_rx.recv().await,
        Some(WorkerControlEnvelope::pair_message(
            pair_id,
            message_id,
            "inspect this",
            Some("client-1".to_string()),
            actor.clone()
        ))
    );
    assert_eq!(
        control_rx.recv().await,
        Some(WorkerControlEnvelope::end_pair(pair_id, actor))
    );
}

#[tokio::test]
async fn in_process_answer_transport_cancel_run_cancels_pending_interviews() {
    let interviewer = Arc::new(ControlInterviewer::new());
    let emitter = Arc::new(fabro_workflow::event::Emitter::new(
        fabro_types::RunId::new(),
    ));
    let steering_hub = Arc::new(fabro_workflow::SteeringHub::new(emitter));
    let transport = RunAnswerTransport::InProcess {
        interviewer:  Arc::clone(&interviewer),
        steering_hub: Arc::clone(&steering_hub),
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
async fn create_run_response_includes_web_url_when_web_enabled() {
    let state = test_app_state_with_options(
        server_settings_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
enabled = true
url = "http://127.0.0.1:32276"
"#,
        ),
        RunLayer::default(),
        5,
    );
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(manifest_body(MINIMAL_DOT))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    let id = body["id"].as_str().expect("id should be a string");
    assert_eq!(
        body["links"]["web"].as_str(),
        Some(format!("http://127.0.0.1:32276/runs/{id}").as_str()),
    );
}

#[tokio::test]
async fn system_repair_runs_lists_catalog_entries_without_projection() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    state
        .store
        .catalog_index()
        .await
        .unwrap()
        .add(&run_id)
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(api("/system/repair/runs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["runs"][0]["run_id"], run_id.to_string());
    let created_at = body["runs"][0]["created_at"]
        .as_str()
        .unwrap()
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    assert_eq!(created_at, run_id.created_at());
    assert!(
        body["runs"][0]["error"]
            .as_str()
            .unwrap()
            .contains("no events"),
        "got: {}",
        body["runs"][0]["error"]
    );
}

#[tokio::test]
async fn create_run_response_omits_web_url_when_web_disabled() {
    let state = test_app_state_with_options(
        server_settings_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
enabled = false
url = "http://127.0.0.1:32276"
"#,
        ),
        RunLayer::default(),
        5,
    );
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(manifest_body(MINIMAL_DOT))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    assert!(
        body.get("web_url").is_none() || body["web_url"].is_null(),
        "web_url should be absent or null when web is disabled, got {body}"
    );
}

#[tokio::test]
async fn create_run_without_explicit_title_returns_deterministic_then_updates_generated_title() {
    let llm = MockServer::start_async().await;
    let title_mock = mock_openai_title_response(&llm, "Generated deploy title", None).await;
    let state = TestAppStateBuilder::new()
        .provider_base_url("openai", llm.url("/v1"))
        .env_lookup(|_| None)
        .build();
    state
        .vault
        .write()
        .await
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let body = post_run_manifest(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let run_id: RunId = body["id"].as_str().unwrap().parse().unwrap();

    assert_eq!(body["title"], "Test");
    wait_for_run_title(&state, run_id, "Generated deploy title").await;
    assert_eq!(title_update_event_count(&state, run_id).await, 1);
    title_mock.assert_async().await;
}

#[tokio::test]
async fn create_run_with_explicit_title_skips_generated_title_work() {
    let llm = MockServer::start_async().await;
    let title_mock = mock_openai_title_response(&llm, "Generated deploy title", None).await;
    let state = TestAppStateBuilder::new()
        .provider_base_url("openai", llm.url("/v1"))
        .env_lookup(|_| None)
        .build();
    state
        .vault
        .write()
        .await
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let mut manifest = minimal_manifest_json(MINIMAL_DOT);
    manifest["title"] = json!("Caller title");

    let body = post_run_manifest(&app, manifest).await;
    let run_id: RunId = body["id"].as_str().unwrap().parse().unwrap();
    // The spawn gate is synchronous in `create_run`, so once the response
    // returns we know no title task was scheduled. No sleep needed.

    assert_eq!(
        state
            .store
            .get_cached_summary(&run_id, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .title,
        "Caller title"
    );
    assert_eq!(title_update_event_count(&state, run_id).await, 0);
    title_mock.assert_calls_async(0).await;
}

#[tokio::test]
async fn create_run_without_ready_llm_provider_skips_generated_title_work() {
    let state = TestAppStateBuilder::new().env_lookup(|_| None).build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let body = post_run_manifest(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let run_id: RunId = body["id"].as_str().unwrap().parse().unwrap();

    assert_eq!(
        state
            .store
            .get_cached_summary(&run_id, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .title,
        "Test"
    );
    assert_eq!(title_update_event_count(&state, run_id).await, 0);
}

#[tokio::test]
async fn generated_title_failure_leaves_deterministic_title_unchanged() {
    let llm = MockServer::start_async().await;
    let title_mock = llm
        .mock_async(|when, then| {
            when.method(POST).path("/v1/responses");
            then.status(500)
                .header("content-type", "application/json")
                .json_body(json!({"error": {"message": "boom"}}));
        })
        .await;
    let state = TestAppStateBuilder::new()
        .provider_base_url("openai", llm.url("/v1"))
        .env_lookup(|_| None)
        .build();
    state
        .vault
        .write()
        .await
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let body = post_run_manifest(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let run_id: RunId = body["id"].as_str().unwrap().parse().unwrap();
    wait_for_mock_hits(&title_mock, 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    assert_eq!(
        state
            .store
            .get_cached_summary(&run_id, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .title,
        "Test"
    );
    assert_eq!(title_update_event_count(&state, run_id).await, 0);
}

#[tokio::test]
async fn generated_title_does_not_overwrite_user_title_edit() {
    let llm = MockServer::start_async().await;
    let title_mock = mock_openai_title_response(
        &llm,
        "Generated deploy title",
        Some(std::time::Duration::from_millis(150)),
    )
    .await;
    let state = TestAppStateBuilder::new()
        .provider_base_url("openai", llm.url("/v1"))
        .env_lookup(|_| None)
        .build();
    state
        .vault
        .write()
        .await
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let body = post_run_manifest(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let run_id: RunId = body["id"].as_str().unwrap().parse().unwrap();
    let patch = Request::builder()
        .method("PATCH")
        .uri(api(&format!("/runs/{run_id}")))
        .header("content-type", "application/json")
        .body(Body::from(json!({"title": "User title"}).to_string()))
        .unwrap();
    let response = app.clone().oneshot(patch).await.unwrap();
    response_json!(response, StatusCode::OK).await;

    wait_for_mock_hits(&title_mock, 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        state
            .store
            .get_cached_summary(&run_id, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .title,
        "User title"
    );
    assert_eq!(title_update_event_count(&state, run_id).await, 1);
}

async fn post_run_manifest(app: &Router, manifest: serde_json::Value) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(manifest.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    response_json!(response, StatusCode::CREATED).await
}

async fn mock_openai_title_response<'a>(
    server: &'a MockServer,
    title: &str,
    delay: Option<std::time::Duration>,
) -> httpmock::Mock<'a> {
    let title = title.to_string();
    server
        .mock_async(move |when, then| {
            when.method(POST).path("/v1/responses");
            let then = then
                .status(200)
                .header("content-type", "application/json")
                .json_body(openai_responses_payload(
                    &json!({ "title": title }).to_string(),
                ));
            if let Some(delay) = delay {
                then.delay(delay);
            }
        })
        .await
}

async fn wait_for_run_title(state: &AppState, run_id: RunId, expected: &str) {
    for _ in 0..50 {
        let title = state
            .store
            .get_cached_summary(&run_id, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .title;
        if title == expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("run {run_id} title did not become {expected:?}");
}

async fn wait_for_mock_hits(mock: &httpmock::Mock<'_>, expected: usize) {
    for _ in 0..50 {
        if mock.calls_async().await >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("mock did not receive {expected} request(s)");
}

async fn title_update_event_count(state: &AppState, run_id: RunId) -> usize {
    let run_store = state.store.open_run(&run_id).await.unwrap();
    run_store
        .list_events()
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.event.event_name() == "run.title.updated")
        .count()
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

#[tokio::test]
async fn validate_endpoint_uses_app_state_catalog_for_model_diagnostics() {
    let llm_catalog_settings: LlmCatalogSettings = toml::from_str(
        r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
    )
    .expect("catalog fixture should parse");
    let state = TestAppStateBuilder::new()
        .llm_catalog_settings(llm_catalog_settings)
        .build();
    let app = crate::test_support::build_test_router(state);
    let dot = r#"digraph Test {
        graph [goal="Test"]
        start [shape=Mdiamond]
        work [model="acme-large", provider="acme", prompt="Do it"]
        exit  [shape=Msquare]
        start -> work -> exit
    }"#;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/validate"))
                .header("content-type", "application/json")
                .body(manifest_body(dot))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let diagnostics = body["workflow"]["diagnostics"].as_array().unwrap();

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic["rule"] != "node_model_known"),
        "custom model/provider should validate against app-state catalog: {body}"
    );
}

#[tokio::test]
async fn validate_endpoint_returns_template_source_coordinates() {
    let app = test_app_with();
    let dot = r#"digraph ValidatePlan {
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        test_imported_prompt [label="moo" prompt="@test.md"]
        start -> test_imported_prompt -> exit
    }"#;
    let manifest = serde_json::json!({
        "version": 1,
        "cwd": "/tmp",
        "target": {
            "identifier": "workflow.fabro",
            "path": "workflow.fabro",
        },
        "workflows": {
            "workflow.fabro": {
                "source": dot,
                "files": {
                    "test.md": {
                        "content": "{{ inputs.foo }}",
                        "ref": {
                            "type": "file_inline",
                            "original": "test.md",
                            "from": "workflow.fabro",
                        },
                    },
                },
            },
        },
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/validate"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&manifest).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let diagnostics = body["workflow"]["diagnostics"].as_array().unwrap();
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["rule"] == "template_undefined_variable")
        .expect("expected template diagnostic");

    assert_eq!(diagnostic["source_path"], "test.md");
    assert_eq!(diagnostic["line"], 1);
    assert_eq!(diagnostic["column"], 4);
    assert!(
        diagnostic["node_id"]
            .as_str()
            .unwrap()
            .contains("test_imported_prompt")
    );
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

async fn create_run_for_target_with_workflow_name(
    app: &Router,
    target_path: &str,
    dot_source: &str,
    workflow_name: &str,
) -> String {
    let mut manifest = manifest_json(target_path, dot_source);
    manifest["workflows"][target_path]["config"] = serde_json::json!({
        "path": "workflow.toml",
        "source": format!("_version = 1\n\n[workflow]\nname = {workflow_name:?}\n"),
    });
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&manifest).unwrap()))
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
    if !matches!(
        events.first(),
        Some(workflow_event::Event::RunCreated { .. })
    ) {
        append_default_run_created(&run_store, run_id).await;
    }
    let needs_running = events.iter().any(|event| {
        matches!(
            event,
            workflow_event::Event::WorkflowRunCompleted { .. }
                | workflow_event::Event::WorkflowRunFailed { .. }
        )
    });
    let has_starting = events
        .iter()
        .any(|event| matches!(event, workflow_event::Event::RunStarting));
    let has_runnable = events
        .iter()
        .any(|event| matches!(event, workflow_event::Event::RunRunnable { .. }));
    let has_running = events
        .iter()
        .any(|event| matches!(event, workflow_event::Event::RunRunning));
    let mut inserted_runnable = has_runnable;
    let mut inserted_starting = has_starting;
    for event in events {
        if !inserted_runnable
            && matches!(
                event,
                workflow_event::Event::RunStarting
                    | workflow_event::Event::RunRunning
                    | workflow_event::Event::RunBlocked { .. }
                    | workflow_event::Event::RunPaused
                    | workflow_event::Event::WorkflowRunCompleted { .. }
                    | workflow_event::Event::WorkflowRunFailed { .. }
            )
        {
            workflow_event::append_event(
                &run_store,
                &run_id,
                &workflow_event::Event::RunRunnable {
                    source: fabro_types::RunRunnableSource::StartRequested,
                    actor:  None,
                },
            )
            .await
            .unwrap();
            inserted_runnable = true;
        }
        if !inserted_starting
            && matches!(
                event,
                workflow_event::Event::RunRunning
                    | workflow_event::Event::RunBlocked { .. }
                    | workflow_event::Event::RunPaused
                    | workflow_event::Event::WorkflowRunCompleted { .. }
                    | workflow_event::Event::WorkflowRunFailed { .. }
            )
        {
            workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunStarting)
                .await
                .unwrap();
            inserted_starting = true;
        }
        if needs_running
            && !has_running
            && matches!(
                event,
                workflow_event::Event::WorkflowRunCompleted { .. }
                    | workflow_event::Event::WorkflowRunFailed { .. }
            )
        {
            workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunning)
                .await
                .unwrap();
        }
        workflow_event::append_event(&run_store, &run_id, event)
            .await
            .unwrap();
    }
}

fn stage_started_event(node_id: &str, handler_type: &str) -> workflow_event::Event {
    workflow_event::Event::StageStarted {
        node_id:      node_id.to_string(),
        name:         node_id.to_string(),
        index:        1,
        handler_type: handler_type.to_string(),
        attempt:      1,
        max_attempts: 1,
    }
}

fn command_started_event(node_id: &str) -> workflow_event::Event {
    workflow_event::Event::CommandStarted {
        node_id:    node_id.to_string(),
        script:     "echo ok".to_string(),
        command:    "echo ok".to_string(),
        language:   "shell".to_string(),
        timeout_ms: None,
    }
}

fn agent_session_activated_event(node_id: &str, visit: u32) -> workflow_event::Event {
    workflow_event::Event::AgentSessionActivated {
        node_id: node_id.to_string(),
        visit,
        session_id: "session-1".to_string(),
        thread_id: None,
        provider: Some("openai".to_string()),
        model: Some("gpt-5.4".to_string()),
        reasoning_effort: None,
        speed: None,
        permission_level: None,
        capabilities: Vec::new(),
    }
}

fn stage_completed_event(node_id: &str) -> workflow_event::Event {
    workflow_event::Event::StageCompleted {
        node_id: node_id.to_string(),
        name: node_id.to_string(),
        index: 1,
        timing: StageTiming::wall_only(42),
        status: "succeeded".to_string(),
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
        attempt: 1,
        max_attempts: 1,
    }
}

fn context_window_event(
    stage: &str,
    visit: u32,
    snapshot: StageContextWindowProjection,
) -> workflow_event::Event {
    workflow_event::Event::Agent {
        stage: stage.to_string(),
        visit,
        event: fabro_agent::AgentEvent::ContextWindowSnapshot(snapshot),
        session_id: Some("session-1".to_string()),
        parent_session_id: None,
        tool_call_id: None,
    }
}

fn context_window_snapshot(
    input_tokens: u64,
    warnings: Vec<StageContextWindowWarning>,
) -> StageContextWindowProjection {
    StageContextWindowProjection {
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        context_window_tokens: 400_000,
        input_tokens,
        usage_percent: input_tokens as f64 * 100.0 / 400_000.0,
        count_method: StageContextWindowCountMethod::ProviderApiScaledBreakdown,
        staleness: StageContextWindowStaleness::Live,
        generated_at: Utc::now(),
        event_seq: None,
        breakdown: vec![StageContextWindowBreakdownItem {
            category:      StageContextWindowCategory::Conversation,
            tokens:        input_tokens,
            usage_percent: input_tokens as f64 * 100.0 / 400_000.0,
        }],
        warnings,
    }
}

async fn append_default_run_created(run_store: &fabro_store::RunDatabase, run_id: RunId) {
    workflow_event::append_event(run_store, &run_id, &workflow_event::Event::RunCreated {
        run_id,
        title: None,
        settings: serde_json::to_value(WorkflowSettings::default()).unwrap(),
        graph: serde_json::to_value(Graph::new("test")).unwrap(),
        workflow_source: None,
        workflow_config: None,
        labels: std::collections::BTreeMap::default(),
        run_dir: "/tmp".to_string(),
        source_directory: None,
        workflow_slug: None,
        db_prefix: None,
        provenance: None,
        manifest_blob: None,
        git: None,
        fork_source_ref: None,
        retried_from: None,
        parent_id: None,
        web_url: None,
    })
    .await
    .unwrap();
}

fn workflow_settings_with_run_notifications(
    run_toml: &str,
    workflow_name: Option<&str>,
) -> WorkflowSettings {
    let mut settings = WorkflowSettings {
        run: fabro_config::RunSettingsBuilder::from_toml(run_toml)
            .expect("run notification settings should resolve"),
        ..WorkflowSettings::default()
    };
    settings.workflow.name = workflow_name.map(str::to_string);
    settings
}

async fn create_slack_notification_run(
    state: &Arc<AppState>,
    run_id: RunId,
    settings: WorkflowSettings,
    graph_name: &str,
    workflow_slug: Option<&str>,
) -> fabro_store::RunDatabase {
    let run_store = state.store.create_run(&run_id).await.unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunCreated {
        run_id,
        title: None,
        settings: serde_json::to_value(settings).unwrap(),
        graph: serde_json::to_value(Graph::new(graph_name)).unwrap(),
        workflow_source: None,
        workflow_config: None,
        labels: std::collections::BTreeMap::default(),
        run_dir: "/tmp".to_string(),
        source_directory: None,
        workflow_slug: workflow_slug.map(str::to_string),
        db_prefix: None,
        provenance: None,
        manifest_blob: None,
        git: None,
        fork_source_ref: None,
        retried_from: None,
        parent_id: None,
        web_url: None,
    })
    .await
    .unwrap();
    run_store
}

async fn append_slack_notification_event(
    run_store: &fabro_store::RunDatabase,
    run_id: RunId,
    event: &workflow_event::Event,
) -> EventEnvelope {
    workflow_event::append_event(run_store, &run_id, event)
        .await
        .unwrap();
    run_store
        .list_events()
        .await
        .unwrap()
        .last()
        .expect("appended event should be present")
        .clone()
}

async fn mock_slack_post<'a>(
    server: &'a MockServer,
    body_includes: Vec<String>,
    ts: &'static str,
) -> httpmock::Mock<'a> {
    server
        .mock_async(move |when, then| {
            let mut when = when
                .method(POST)
                .path("/chat.postMessage")
                .header("authorization", "Bearer xoxb-test");
            for part in body_includes {
                when = when.body_includes(part);
            }
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "ok": true,
                    "channel": "C123",
                    "ts": ts,
                }));
        })
        .await
}

fn slack_lifecycle_service(base_url: String, default_channel: Option<&str>) -> SlackService {
    SlackService {
        client:          fabro_slack::client::SlackClient::with_api_base_and_http(
            "xoxb-test".to_string(),
            base_url,
            fabro_http::test_http_client().expect("test HTTP client should build"),
        ),
        app_token:       "xapp-test".to_string(),
        default_channel: default_channel.map(str::to_string),
        posted_messages: StdArc::new(StdMutex::new(HashMap::new())),
        thread_registry: StdArc::new(ThreadRegistry::new()),
    }
}

fn workflow_run_started_event(run_id: RunId) -> workflow_event::Event {
    workflow_event::Event::WorkflowRunStarted {
        name: "run.started event name".to_string(),
        run_id,
        base_branch: None,
        base_sha: None,
        run_branch: None,
        worktree_dir: None,
        goal: None,
    }
}

#[tokio::test]
async fn slack_lifecycle_run_started_posts_for_matching_enabled_route() {
    let server = MockServer::start_async().await;
    let post = mock_slack_post(
        &server,
        vec![
            r##""channel":"#deploys""##.to_string(),
            "Fabro run started".to_string(),
            "Deploy workflow".to_string(),
            "Open in Fabro".to_string(),
        ],
        "100.1",
    )
    .await;
    let state = test_app_state();
    let service = slack_lifecycle_service(server.base_url(), None);
    let run_id = fixtures::RUN_1;
    let settings = workflow_settings_with_run_notifications(
        r##"
[run.notifications.deploys]
enabled = true
provider = "slack"
events = ["run.started", "run.completed", "run.failed"]

[run.notifications.deploys.slack]
channel = "#deploys"
"##,
        Some("Deploy workflow"),
    );
    let run_store =
        create_slack_notification_run(&state, run_id, settings, "deploy-graph", Some("deploy"))
            .await;
    let envelope =
        append_slack_notification_event(&run_store, run_id, &workflow_run_started_event(run_id))
            .await;

    service
        .handle_event(
            state.as_ref(),
            &envelope,
            Some("https://fabro.example/runs/run-1"),
        )
        .await;

    post.assert_async().await;
    assert!(
        service
            .posted_messages
            .lock()
            .expect("posted messages lock poisoned")
            .is_empty(),
        "lifecycle posts must not use interview message state"
    );
}

#[tokio::test]
async fn slack_lifecycle_run_completed_posts_result_and_duration() {
    let server = MockServer::start_async().await;
    let post = mock_slack_post(
        &server,
        vec![
            r##""channel":"#deploys""##.to_string(),
            "Fabro run completed".to_string(),
            "succeeded — completed".to_string(),
            "1m 5s".to_string(),
        ],
        "100.2",
    )
    .await;
    let state = test_app_state();
    let service = slack_lifecycle_service(server.base_url(), None);
    let run_id = fixtures::RUN_1;
    let settings = workflow_settings_with_run_notifications(
        r##"
[run.notifications.deploys]
enabled = true
provider = "slack"
events = ["run.completed"]

[run.notifications.deploys.slack]
channel = "#deploys"
"##,
        Some("Deploy workflow"),
    );
    let run_store = create_slack_notification_run(&state, run_id, settings, "deploy", None).await;
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunnable {
        source: fabro_types::RunRunnableSource::StartRequested,
        actor:  None,
    })
    .await
    .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunStarting)
        .await
        .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunning)
        .await
        .unwrap();
    let envelope = append_slack_notification_event(
        &run_store,
        run_id,
        &workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(65_432),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    )
    .await;

    service.handle_event(state.as_ref(), &envelope, None).await;

    post.assert_async().await;
}

#[tokio::test]
async fn slack_lifecycle_run_failed_posts_failure_result_message_and_duration() {
    let server = MockServer::start_async().await;
    let post = mock_slack_post(
        &server,
        vec![
            r##""channel":"#deploys""##.to_string(),
            "Fabro run failed".to_string(),
            "workflow_error — command &lt;failed&gt; &amp; exited".to_string(),
            "1.2s".to_string(),
        ],
        "100.3",
    )
    .await;
    let state = test_app_state();
    let service = slack_lifecycle_service(server.base_url(), None);
    let run_id = fixtures::RUN_1;
    let settings = workflow_settings_with_run_notifications(
        r##"
[run.notifications.deploys]
enabled = true
provider = "slack"
events = ["run.failed"]

[run.notifications.deploys.slack]
channel = "#deploys"
"##,
        Some("Deploy workflow"),
    );
    let run_store = create_slack_notification_run(&state, run_id, settings, "deploy", None).await;
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunnable {
        source: fabro_types::RunRunnableSource::StartRequested,
        actor:  None,
    })
    .await
    .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunStarting)
        .await
        .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunning)
        .await
        .unwrap();
    let envelope = append_slack_notification_event(
        &run_store,
        run_id,
        &workflow_event::Event::WorkflowRunFailed {
            failure:              fabro_types::RunFailure {
                reason: fabro_types::FailureReason::WorkflowError,
                detail: FailureDetail::new(
                    "command <failed> & exited",
                    FailureCategory::Deterministic,
                ),
            },
            timing:               fabro_types::RunTiming::wall_only(1_234),
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    )
    .await;

    service.handle_event(state.as_ref(), &envelope, None).await;

    post.assert_async().await;
}

#[tokio::test]
async fn slack_lifecycle_skips_non_matching_events_and_disabled_routes() {
    let server = MockServer::start_async().await;
    let unexpected = mock_slack_post(&server, Vec::new(), "100.4").await;
    let state = test_app_state();
    let service = slack_lifecycle_service(server.base_url(), None);
    let run_id = fixtures::RUN_1;
    let settings = workflow_settings_with_run_notifications(
        r##"
[run.notifications.disabled]
enabled = false
provider = "slack"
events = ["run.started"]

[run.notifications.disabled.slack]
channel = "#deploys"

[run.notifications.stage]
enabled = true
provider = "slack"
events = ["stage.completed"]

[run.notifications.stage.slack]
channel = "#deploys"
"##,
        Some("Deploy workflow"),
    );
    let run_store = create_slack_notification_run(&state, run_id, settings, "deploy", None).await;
    let envelope =
        append_slack_notification_event(&run_store, run_id, &workflow_run_started_event(run_id))
            .await;

    service.handle_event(state.as_ref(), &envelope, None).await;

    unexpected.assert_calls_async(0).await;
}

#[tokio::test]
async fn slack_lifecycle_missing_channel_is_skipped_without_blocking_other_routes() {
    let server = MockServer::start_async().await;
    let post = mock_slack_post(
        &server,
        vec![
            r##""channel":"#ops""##.to_string(),
            "Fabro run started".to_string(),
        ],
        "100.5",
    )
    .await;
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        fabro_config::RunLayer::default(),
        5,
        |name| match name {
            "SLACK_ROUTE_CHANNEL" => Some("#ops".to_string()),
            _ => None,
        },
    );
    let service = slack_lifecycle_service(server.base_url(), None);
    let run_id = fixtures::RUN_1;
    let settings = workflow_settings_with_run_notifications(
        r#"
[run.notifications.missing]
enabled = true
provider = "slack"
events = ["run.started"]

[run.notifications.unresolved]
enabled = true
provider = "slack"
events = ["run.started"]

[run.notifications.unresolved.slack]
channel = "{{ env.MISSING_SLACK_CHANNEL }}"

[run.notifications.valid]
enabled = true
provider = "slack"
events = ["run.started"]

[run.notifications.valid.slack]
channel = "{{ env.SLACK_ROUTE_CHANNEL }}"
"#,
        Some("Deploy workflow"),
    );
    let run_store = create_slack_notification_run(&state, run_id, settings, "deploy", None).await;
    let envelope =
        append_slack_notification_event(&run_store, run_id, &workflow_run_started_event(run_id))
            .await;

    service.handle_event(state.as_ref(), &envelope, None).await;

    post.assert_async().await;
}

#[tokio::test]
async fn slack_lifecycle_uses_prior_pull_request_created_details() {
    let server = MockServer::start_async().await;
    let post = mock_slack_post(
        &server,
        vec![
            "Fabro run completed".to_string(),
            "https://github.com/fabro-sh/fabro/pull/42".to_string(),
            "#42".to_string(),
            "Ship &lt;prod&gt; &amp; notify".to_string(),
        ],
        "100.6",
    )
    .await;
    let state = test_app_state();
    let service = slack_lifecycle_service(server.base_url(), None);
    let run_id = fixtures::RUN_1;
    let settings = workflow_settings_with_run_notifications(
        r##"
[run.notifications.deploys]
enabled = true
provider = "slack"
events = ["run.completed"]

[run.notifications.deploys.slack]
channel = "#deploys"
"##,
        Some("Deploy workflow"),
    );
    let run_store = create_slack_notification_run(&state, run_id, settings, "deploy", None).await;
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunnable {
        source: fabro_types::RunRunnableSource::StartRequested,
        actor:  None,
    })
    .await
    .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunStarting)
        .await
        .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunning)
        .await
        .unwrap();
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::PullRequestCreated {
            pr_url:      "https://github.com/fabro-sh/fabro/pull/42".to_string(),
            pr_number:   42,
            owner:       "fabro-sh".to_string(),
            repo:        "fabro".to_string(),
            base_branch: "main".to_string(),
            head_branch: "fabro/run/test".to_string(),
            title:       "Ship <prod> & notify".to_string(),
            draft:       false,
        },
    )
    .await
    .unwrap();
    let envelope = append_slack_notification_event(
        &run_store,
        run_id,
        &workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    )
    .await;

    service.handle_event(state.as_ref(), &envelope, None).await;

    post.assert_async().await;
}

#[tokio::test]
async fn slack_interviews_keep_state_separate_from_lifecycle_notifications() {
    let server = MockServer::start_async().await;
    let interview_post = mock_slack_post(
        &server,
        vec![
            r##""channel":"#reviews""##.to_string(),
            "Answer deploy question".to_string(),
        ],
        "200.1",
    )
    .await;
    let lifecycle_post = mock_slack_post(
        &server,
        vec![
            r##""channel":"#deploys""##.to_string(),
            "Fabro run started".to_string(),
        ],
        "200.2",
    )
    .await;
    let state = test_app_state();
    let service = slack_lifecycle_service(server.base_url(), Some("#reviews"));
    let run_id = fixtures::RUN_1;
    let settings = workflow_settings_with_run_notifications(
        r##"
[run.notifications.deploys]
enabled = true
provider = "slack"
events = ["run.started"]

[run.notifications.deploys.slack]
channel = "#deploys"
"##,
        Some("Deploy workflow"),
    );
    let run_store = create_slack_notification_run(&state, run_id, settings, "deploy", None).await;
    let lifecycle_envelope =
        append_slack_notification_event(&run_store, run_id, &workflow_run_started_event(run_id))
            .await;
    let interview_envelope = append_slack_notification_event(
        &run_store,
        run_id,
        &workflow_event::Event::InterviewStarted {
            question_id:     "q-1".to_string(),
            question:        "Answer deploy question".to_string(),
            stage:           "review".to_string(),
            question_type:   "freeform".to_string(),
            options:         Vec::new(),
            allow_freeform:  true,
            timeout_seconds: None,
            context_display: None,
        },
    )
    .await;

    service
        .handle_event(state.as_ref(), &lifecycle_envelope, None)
        .await;
    assert!(
        service
            .posted_messages
            .lock()
            .expect("posted messages lock poisoned")
            .is_empty(),
        "lifecycle notification should not record interview metadata"
    );
    assert!(
        service.thread_registry.resolve("200.2").is_none(),
        "lifecycle notification should not register answer threads"
    );

    service
        .handle_event(state.as_ref(), &interview_envelope, None)
        .await;

    lifecycle_post.assert_async().await;
    interview_post.assert_async().await;
    assert!(
        service
            .posted_messages
            .lock()
            .expect("posted messages lock poisoned")
            .contains_key(&(run_id, "q-1".to_string())),
        "interview posts should retain interview state"
    );
    assert!(
        service.thread_registry.resolve("200.1").is_some(),
        "freeform interview posts should register reply threads"
    );
}

#[tokio::test]
async fn persist_cancelled_run_status_ignores_already_terminal_runs() {
    let state = test_app_state();
    let run_id = fixtures::RUN_1;
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    persist_cancelled_run_status(state.as_ref(), run_id)
        .await
        .unwrap();

    let run_store = state.store.open_run(&run_id).await.unwrap();
    let projection = run_store.state().await.unwrap();
    assert_eq!(projection.status, RunStatus::Succeeded {
        reason: SuccessReason::Completed,
    });
    assert!(!run_store.list_events().await.unwrap().iter().any(|event| {
        matches!(
            event.event.body,
            EventBody::RunFailed(ref props) if props.failure.reason == FailureReason::Cancelled
        )
    }));
}

#[tokio::test]
async fn delete_terminal_managed_run_does_not_send_cancel_signal() {
    let state = test_app_state();
    let run_id = fixtures::RUN_1;
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let cancel_token = CancellationToken::new();
    let mut run = managed_run(
        MINIMAL_DOT.to_string(),
        RunStatus::Running,
        Utc::now(),
        run_dir,
        RunExecutionMode::Start,
    );
    run.cancel_token = Some(cancel_token.clone());
    let (cancel_tx, _cancel_rx) = oneshot::channel();
    run.cancel_tx = Some(cancel_tx);
    state
        .runs
        .lock()
        .expect("runs lock poisoned")
        .insert(run_id, run);

    delete_run_internal(&state, run_id, true).await.unwrap();

    assert!(!cancel_token.is_cancelled());
}

/// Append a stage lifecycle event with an explicit `StageScope`, so the
/// stored envelope carries the full `stage_id` (`node_id@visit`). The bare
/// [`workflow_event::append_event`] helper only writes `node_id` because
/// stage lifecycle variants don't carry visit in their payload — production
/// always emits via `Emitter::emit_scoped`.
async fn append_scoped_stage_event(
    state: &Arc<AppState>,
    run_id: RunId,
    node_id: &str,
    visit: u32,
    event: &workflow_event::Event,
) {
    let scope = fabro_workflow::event::StageScope {
        node_id: node_id.to_string(),
        visit,
        parallel_group_id: None,
        parallel_branch_id: None,
    };
    let stored = fabro_workflow::event::to_run_event_at(&run_id, event, Utc::now(), Some(&scope));
    let payload = fabro_workflow::event::build_redacted_event_payload(&stored, &run_id).unwrap();
    let run_store = state.store.open_run(&run_id).await.unwrap();
    run_store.append_event(&payload).await.unwrap();
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
    ])
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "setup",
        1,
        &workflow_event::Event::StageStarted {
            node_id:      "setup".to_string(),
            name:         "Setup".to_string(),
            index:        0,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "setup",
        1,
        &workflow_event::Event::StageCompleted {
            node_id: "setup".to_string(),
            name: "Setup".to_string(),
            index: 0,
            timing: fabro_types::StageTiming::wall_only(5),
            status: "succeeded".to_string(),
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
            attempt: 1,
            max_attempts: 1,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageStarted {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        1,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 3,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageFailed {
            node_id:    "work".to_string(),
            name:       "Work".to_string(),
            index:      1,
            failure:    FailureDetail::new("try again", FailureCategory::TransientInfra),
            will_retry: true,
            timing:     fabro_types::StageTiming::wall_only(10),
            billing:    None,
            actor:      None,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageRetrying {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        1,
            attempt:      2,
            max_attempts: 3,
            delay_ms:     100,
        },
    )
    .await;

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
    assert_eq!(stage_status(&body, "setup@1"), "succeeded");
    assert_eq!(stage_status(&body, "work@1"), "retrying");

    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageCompleted {
            node_id: "work".to_string(),
            name: "Work".to_string(),
            index: 1,
            timing: fabro_types::StageTiming::wall_only(25),
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
    .await;

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
    assert_eq!(stage_status(&body, "work@1"), "partially_succeeded");
}

fn stage_entry<'a>(body: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stage| stage["id"] == id)
        .unwrap_or_else(|| panic!("stage {id} not found in {body:#?}"))
}

#[tokio::test]
async fn list_run_stages_includes_stage_model_usage() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "prompt",
        1,
        &workflow_event::Event::StageStarted {
            node_id:      "prompt".to_string(),
            name:         "Prompt".to_string(),
            index:        0,
            handler_type: "prompt".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "prompt",
        1,
        &workflow_event::Event::Prompt {
            stage:            "prompt".to_string(),
            visit:            1,
            text:             "Summarize".to_string(),
            mode:             Some(StageModelUsage::MODE_PROMPT.to_string()),
            provider:         Some("openai".to_string()),
            model:            Some("gpt-5.5".to_string()),
            reasoning_effort: Some(ReasoningEffort::High),
            speed:            Some(Speed::Fast),
        },
    )
    .await;

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
    assert_eq!(
        stage_entry(&body, "prompt@1")["provider_used"],
        json!({
            "mode": "prompt",
            "provider": "openai",
            "model": "gpt-5.5",
            "reasoning_effort": "high",
            "speed": "fast"
        })
    );
}

fn test_billed_usage(
    model_id: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> fabro_model::BilledModelUsage {
    serde_json::from_value(json!({
        "input": {
            "usage": {
                "model": {
                    "provider": "openai",
                    "model_id": model_id
                },
                "tokens": {
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens
                }
            },
            "facts": { "algorithm": "openai" }
        },
        "total_usd_micros": input_tokens + output_tokens
    }))
    .unwrap()
}

#[tokio::test]
async fn list_run_stages_distinguishes_visits() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let mut graph = Graph::new("test");
    let mut verify = Node::new("verify");
    verify
        .attrs
        .insert("type".to_string(), AttrValue::String("command".to_string()));
    graph.nodes.insert("verify".to_string(), verify);

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunCreated {
            run_id,
            title: None,
            settings: serde_json::to_value(fabro_types::WorkflowSettings::default()).unwrap(),
            graph: serde_json::to_value(&graph).unwrap(),
            workflow_source: None,
            workflow_config: None,
            labels: std::collections::BTreeMap::default(),
            run_dir: String::new(),
            source_directory: None,
            workflow_slug: Some("test".to_string()),
            db_prefix: None,
            provenance: None,
            manifest_blob: None,
            git: None,
            fork_source_ref: None,
            retried_from: None,
            parent_id: None,
            web_url: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    // First visit of `verify` — failed.
    append_scoped_stage_event(
        &state,
        run_id,
        "verify",
        1,
        &workflow_event::Event::StageStarted {
            node_id:      "verify".to_string(),
            name:         "Verify".to_string(),
            index:        1,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "verify",
        1,
        &workflow_event::Event::StageCompleted {
            node_id: "verify".to_string(),
            name: "Verify".to_string(),
            index: 1,
            timing: fabro_types::StageTiming::wall_only(1500),
            status: "failed".to_string(),
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
            attempt: 1,
            max_attempts: 1,
        },
    )
    .await;

    // Second visit of `verify` — running.
    append_scoped_stage_event(
        &state,
        run_id,
        "verify",
        2,
        &workflow_event::Event::StageStarted {
            node_id:      "verify".to_string(),
            name:         "Verify".to_string(),
            index:        1,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
    )
    .await;

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

    let data = body["data"].as_array().unwrap();
    let verify_entries: Vec<_> = data.iter().filter(|s| s["node_id"] == "verify").collect();
    assert_eq!(verify_entries.len(), 2, "expected two verify visits");

    let first = stage_entry(&body, "verify@1");
    assert_eq!(first["node_id"], "verify");
    assert_eq!(first["visit"], 1);
    assert_eq!(first["handler"], "command");
    assert_eq!(first["status"], "failed");
    assert_eq!(first["wall_time_ms"], 1500);

    let second = stage_entry(&body, "verify@2");
    assert_eq!(second["node_id"], "verify");
    assert_eq!(second["visit"], 2);
    assert_eq!(second["handler"], "command");
    assert_eq!(second["status"], "running");

    // Old `dot_id` field must be gone.
    assert!(first.get("dot_id").is_none(), "dot_id should be removed");
}

/// `checkpoint.completed_nodes` records every visit, so a looped node appears
/// once per re-entry. Billing must dedup so a retried node renders as one row
/// and `runtime_secs` is summed across all visits exactly once.
#[tokio::test]
async fn run_billing_dedups_retried_nodes_and_sums_their_durations() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    // Visit 1 of `verify` — completed in 1.5s.
    append_scoped_stage_event(
        &state,
        run_id,
        "verify",
        1,
        &workflow_event::Event::StageCompleted {
            node_id: "verify".to_string(),
            name: "Verify".to_string(),
            index: 1,
            timing: fabro_types::StageTiming::wall_only(1500),
            status: "failed".to_string(),
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
            attempt: 1,
            max_attempts: 1,
        },
    )
    .await;

    // Visit 2 of `verify` — completed in 0.8s.
    append_scoped_stage_event(
        &state,
        run_id,
        "verify",
        2,
        &workflow_event::Event::StageCompleted {
            node_id: "verify".to_string(),
            name: "Verify".to_string(),
            index: 1,
            timing: fabro_types::StageTiming::wall_only(800),
            status: "succeeded".to_string(),
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
            attempt: 1,
            max_attempts: 1,
        },
    )
    .await;

    // Checkpoint records `verify` twice (once per visit) — this is what makes
    // the dedup necessary.
    let run_store = state.store.open_run(&run_id).await.unwrap();
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::CheckpointCompleted {
            node_id: "verify".to_string(),
            status: "running".to_string(),
            current_node: "verify".to_string(),
            completed_nodes: vec!["verify".to_string(), "verify".to_string()],
            node_retries: std::collections::BTreeMap::new(),
            context_values: std::collections::BTreeMap::new(),
            node_outcomes: std::collections::BTreeMap::from([(
                "verify".to_string(),
                Outcome::default(),
            )]),
            next_node_id: Some("done".to_string()),
            git_commit_sha: None,
            loop_failure_signatures: std::collections::BTreeMap::new(),
            restart_failure_signatures: std::collections::BTreeMap::new(),
            node_visits: std::collections::BTreeMap::from([("verify".to_string(), 2usize)]),
            diff: None,
            diff_summary: None,
        },
    )
    .await
    .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/billing")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    let stages = body["stages"].as_array().unwrap();
    assert_eq!(
        stages.len(),
        1,
        "expected one row for the retried verify node"
    );
    assert_eq!(stages[0]["stage"]["id"], "verify");
    // Duration on the row is the sum across visits (1.5s + 0.8s = 2.3s).
    assert!(
        stages[0]["timing"]["wall_time_ms"].as_u64().unwrap() == 2300,
        "row runtime_secs should sum visits, got {}",
        stages[0]["timing"]["wall_time_ms"]
    );

    // Totals must not double-count: a single 2.3s, not 4.6s.
    assert!(
        body["totals"]["timing"]["wall_time_ms"].as_u64().unwrap() == 2300,
        "totals.runtime_secs should sum visits exactly once, got {}",
        body["totals"]["timing"]["wall_time_ms"]
    );
}

#[tokio::test]
async fn run_billing_sums_usage_across_retry_visits_and_uses_latest_model() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let failed_usage = test_billed_usage("gpt-old", 100, 10);
    let success_usage = test_billed_usage("gpt-new", 200, 20);

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    append_scoped_stage_event(
        &state,
        run_id,
        "verify",
        1,
        &workflow_event::Event::StageFailed {
            node_id:    "verify".to_string(),
            name:       "Verify".to_string(),
            index:      1,
            failure:    FailureDetail::new("try again", FailureCategory::TransientInfra),
            will_retry: true,
            timing:     fabro_types::StageTiming::wall_only(1200),
            billing:    Some(failed_usage),
            actor:      None,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "verify",
        2,
        &workflow_event::Event::StageCompleted {
            node_id: "verify".to_string(),
            name: "Verify".to_string(),
            index: 1,
            timing: fabro_types::StageTiming::wall_only(800),
            status: "succeeded".to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing: Some(success_usage.clone()),
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
            max_attempts: 2,
        },
    )
    .await;

    let mut latest_outcome: Outcome<Option<fabro_model::BilledModelUsage>> = Outcome::success();
    latest_outcome.usage = Some(success_usage);
    latest_outcome.timing = Some(fabro_types::StageTiming::wall_only(800));
    let run_store = state.store.open_run(&run_id).await.unwrap();
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::CheckpointCompleted {
            node_id: "verify".to_string(),
            status: "running".to_string(),
            current_node: "verify".to_string(),
            completed_nodes: vec!["verify".to_string(), "verify".to_string()],
            node_retries: std::collections::BTreeMap::from([("verify".to_string(), 2)]),
            context_values: std::collections::BTreeMap::new(),
            node_outcomes: std::collections::BTreeMap::from([(
                "verify".to_string(),
                latest_outcome,
            )]),
            next_node_id: None,
            git_commit_sha: None,
            loop_failure_signatures: std::collections::BTreeMap::new(),
            restart_failure_signatures: std::collections::BTreeMap::new(),
            node_visits: std::collections::BTreeMap::from([("verify".to_string(), 2usize)]),
            diff: None,
            diff_summary: None,
        },
    )
    .await
    .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/billing")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    let stages = body["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0]["stage"]["id"], "verify");
    assert_eq!(stages[0]["model"]["provider"], "openai");
    assert_eq!(stages[0]["model"]["model_id"], "gpt-new");
    assert_eq!(stages[0]["billing"]["input_tokens"], 300);
    assert_eq!(stages[0]["billing"]["output_tokens"], 30);
    assert_eq!(stages[0]["billing"]["total_usd_micros"], 330);
    assert!(stages[0]["timing"]["wall_time_ms"].as_u64().unwrap() == 2000);

    assert_eq!(body["totals"]["input_tokens"], 300);
    assert_eq!(body["totals"]["output_tokens"], 30);
    assert_eq!(body["totals"]["total_usd_micros"], 330);
    assert!(body["totals"]["timing"]["wall_time_ms"].as_u64().unwrap() == 2000);

    let by_model = body["by_model"].as_array().unwrap();
    assert_eq!(by_model.len(), 2);
    let old_model = by_model
        .iter()
        .find(|entry| entry["model"]["model_id"] == "gpt-old")
        .unwrap();
    let new_model = by_model
        .iter()
        .find(|entry| entry["model"]["model_id"] == "gpt-new")
        .unwrap();
    assert_eq!(old_model["model"]["provider"], "openai");
    assert_eq!(new_model["model"]["provider"], "openai");
    assert_eq!(old_model["stages"], 1);
    assert_eq!(old_model["billing"]["input_tokens"], 100);
    assert_eq!(new_model["stages"], 1);
    assert_eq!(new_model["billing"]["input_tokens"], 200);
}

#[tokio::test]
async fn list_run_stages_shows_retrying_after_failed_event() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageStarted {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        0,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 3,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageFailed {
            node_id:    "work".to_string(),
            name:       "Work".to_string(),
            index:      0,
            failure:    FailureDetail::new("flake", FailureCategory::TransientInfra),
            will_retry: true,
            timing:     fabro_types::StageTiming::wall_only(5),
            billing:    None,
            actor:      None,
        },
    )
    .await;
    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageRetrying {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        0,
            attempt:      2,
            max_attempts: 3,
            delay_ms:     50,
        },
    )
    .await;

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
    assert_eq!(stage_status(&body, "work@1"), "retrying");
}

#[tokio::test]
async fn list_run_stages_shows_retrying_when_failed_will_retry() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageStarted {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        0,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 3,
        },
    )
    .await;
    // Only StageFailed, no StageRetrying yet — should still render retrying
    // because props.will_retry is true.
    append_scoped_stage_event(
        &state,
        run_id,
        "work",
        1,
        &workflow_event::Event::StageFailed {
            node_id:    "work".to_string(),
            name:       "Work".to_string(),
            index:      0,
            failure:    FailureDetail::new("flake", FailureCategory::TransientInfra),
            will_retry: true,
            timing:     fabro_types::StageTiming::wall_only(5),
            billing:    None,
            actor:      None,
        },
    )
    .await;

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
    assert_eq!(stage_status(&body, "work@1"), "retrying");
}

#[tokio::test]
async fn run_billing_retried_node_then_succeeded_emits_one_row_with_final_attempt_duration() {
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
            index:        0,
            handler_type: "command".to_string(),
            attempt:      1,
            max_attempts: 3,
        },
        workflow_event::Event::StageFailed {
            node_id:    "work".to_string(),
            name:       "Work".to_string(),
            index:      0,
            failure:    FailureDetail::new("transient", FailureCategory::TransientInfra),
            will_retry: true,
            timing:     fabro_types::StageTiming::wall_only(10),
            billing:    None,
            actor:      None,
        },
        workflow_event::Event::StageRetrying {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        0,
            attempt:      2,
            max_attempts: 3,
            delay_ms:     0,
        },
        workflow_event::Event::StageStarted {
            node_id:      "work".to_string(),
            name:         "Work".to_string(),
            index:        0,
            handler_type: "command".to_string(),
            attempt:      2,
            max_attempts: 3,
        },
        workflow_event::Event::StageCompleted {
            node_id: "work".to_string(),
            name: "Work".to_string(),
            index: 0,
            timing: fabro_types::StageTiming::wall_only(25),
            status: "succeeded".to_string(),
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
    ])
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/billing")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let stages = body["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 1, "retry collapses to one row per node_id");
    let row = &stages[0];
    assert_eq!(row["stage"]["id"], "work");
    assert_eq!(
        row["state"], "succeeded",
        "final state mirrors the latest StageCompleted"
    );
    let runtime = row["timing"]["wall_time_ms"].as_u64().unwrap();
    assert_eq!(
        runtime, 25,
        "runtime should equal final attempt's 25ms, got {runtime}"
    );
}

fn revisit_test_started(node_id: &str) -> workflow_event::Event {
    workflow_event::Event::StageStarted {
        node_id:      node_id.to_string(),
        name:         node_id.to_string(),
        index:        0,
        handler_type: "command".to_string(),
        attempt:      1,
        max_attempts: 1,
    }
}

fn revisit_test_completed_with_visit(
    node_id: &str,
    duration_ms: u64,
    visit: usize,
) -> workflow_event::Event {
    let mut node_visits = std::collections::BTreeMap::new();
    node_visits.insert(node_id.to_string(), visit);
    workflow_event::Event::StageCompleted {
        node_id: node_id.to_string(),
        name: node_id.to_string(),
        index: 0,
        timing: fabro_types::StageTiming::wall_only(duration_ms),
        status: "succeeded".to_string(),
        preferred_label: None,
        suggested_next_ids: Vec::new(),
        billing: None,
        failure: None,
        notes: None,
        files_touched: Vec::new(),
        context_updates: None,
        jump_to_node: None,
        context_values: None,
        node_visits: Some(node_visits),
        loop_failure_signatures: None,
        restart_failure_signatures: None,
        response: None,
        attempt: 1,
        max_attempts: 1,
    }
}

#[tokio::test]
async fn run_billing_revisited_node_collapses_to_two_rows_with_summed_visit_duration() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        // A → B → A loop. Per-visit `node_visits` payload steers the reducer
        // to attribute each StageCompleted to the right visit.
        revisit_test_started("a"),
        revisit_test_completed_with_visit("a", 1, 1),
        revisit_test_started("b"),
        revisit_test_completed_with_visit("b", 2, 1),
        revisit_test_started("a"),
        revisit_test_completed_with_visit("a", 99, 2),
    ])
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/billing")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let stages = body["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2, "two distinct node_ids → two rows");
    assert_eq!(
        stages[0]["stage"]["id"], "a",
        "A appeared first → A's row first"
    );
    assert_eq!(stages[1]["stage"]["id"], "b");
    let a_runtime = stages[0]["timing"]["wall_time_ms"].as_u64().unwrap();
    assert_eq!(
        a_runtime, 100,
        "A should sum both visit durations (1ms + 99ms), got {a_runtime}"
    );
    let b_runtime = stages[1]["timing"]["wall_time_ms"].as_u64().unwrap();
    assert_eq!(
        b_runtime, 2,
        "B should carry its single visit's duration (2ms), got {b_runtime}"
    );
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

async fn create_unreadable_durable_run(state: &Arc<AppState>, run_id: RunId) {
    let run_store = state.store.create_run(&run_id).await.unwrap();
    append_default_run_created(&run_store, run_id).await;
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunnable {
        source: fabro_types::RunRunnableSource::StartRequested,
        actor:  None,
    })
    .await
    .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunStarting)
        .await
        .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunning)
        .await
        .unwrap();
    let payload = fabro_store::EventPayload::new(
        json!({
            "id": "evt-unreadable-run-completed",
            "ts": "2026-05-05T20:46:33Z",
            "run_id": run_id,
            "event": "run.completed",
            "properties": {
                "timing": {
                    "wall_time_ms": 1,
                    "inference_time_ms": 0,
                    "tool_time_ms": 0,
                    "active_time_ms": 0
                },
                "artifact_count": 0,
                "status": "legacy-status",
                "reason": "completed",
            },
        }),
        &run_id,
    )
    .unwrap();
    let err = run_store
        .append_event(&payload)
        .await
        .expect_err("invalid projection event should be persisted but rejected by projection");
    assert!(
        err.to_string().contains("invalid completed stage status"),
        "unexpected projection error: {err}"
    );
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
    create_github_token_app_state_with_env_lookup_and_llm_catalog_settings(
        token,
        github_api_base_url,
        env_lookup,
        LlmCatalogSettings::default(),
    )
}

fn create_github_token_app_state_with_env_lookup_and_llm_catalog_settings(
    token: Option<&str>,
    github_api_base_url: Option<String>,
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    llm_catalog_settings: LlmCatalogSettings,
) -> Arc<AppState> {
    let (store, artifact_store) = test_store_bundle();
    let vault_path = test_secret_store_path();
    let server_env_path = vault_path.with_file_name("server.env");
    let config = AppStateConfig {
        resolved_settings: resolved_runtime_settings_for_tests(
            github_token_settings(),
            RunLayer::default(),
            llm_catalog_settings,
        ),
        registry_factory_override: None,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        vault_path,
        server_secrets: load_test_server_secrets(server_env_path, HashMap::new()),
        env_lookup: Arc::new(env_lookup),
        github_api_base_url,
        active_config_path: tempfile::tempdir().unwrap().path().join("settings.toml"),
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
        shutdown: tokio_util::sync::CancellationToken::new(),
    };
    let state = build_app_state(config).expect("test app state should build");
    if let Some(token) = token {
        state
            .vault
            .try_write()
            .expect("test vault should not already be locked")
            .set("GITHUB_TOKEN", token, SecretType::Token, None)
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
    Box::pin(create_completed_run_ready_for_pull_request(
        &state,
        run_id,
        repo_origin_url,
        Some("main"),
        Some("fabro/run/42"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+fn shipped() {}\n",
    ))
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

async fn create_run_with_linked_pull_request_record(
    state: &Arc<AppState>,
    run_id: RunId,
    pull_request: PullRequestLink,
) {
    create_durable_run_with_events(state, run_id, &[workflow_event::Event::PullRequestLinked {
        pull_request,
    }])
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
        graph_source: None,
        workflow_slug: Some("test".to_string()),
        source_directory: Some("/tmp/project".to_string()),
        git: git.clone(),
        labels: HashMap::new(),
        provenance: None,
        manifest_blob: None,
        definition_blob: None,
        fork_source_ref: None,
    };

    create_durable_run_with_events(state, run_id, &[
        workflow_event::Event::RunCreated {
            run_id,
            title: None,
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
            retried_from: None,
            parent_id: None,
            web_url: None,
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
            timing:               fabro_types::RunTiming::wall_only(1),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          Some(final_patch.to_string()),
            diff_summary:         None,
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
async fn list_models_marks_configured_false_when_provider_cannot_register() {
    let llm_catalog_settings: LlmCatalogSettings = toml::from_str(
        r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
priority = 120

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
    )
    .expect("catalog fixture should parse");
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .env_lookup(|name| (name == "ACME_API_KEY").then(|| "acme-key".to_string()))
        .llm_catalog_settings(llm_catalog_settings)
        .build();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("GET")
        .uri(api("/models?provider=acme"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let models = body["data"].as_array().unwrap();

    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "acme-large");
    assert_eq!(models[0]["configured"].as_bool(), Some(false));
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
async fn list_models_unknown_provider_returns_empty_page() {
    let app = test_app_with();

    let req = Request::builder()
        .method("GET")
        .uri(api("/models?provider=missing-provider"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
    assert_eq!(body["meta"]["has_more"].as_bool(), Some(false));
}

#[tokio::test]
async fn list_models_uses_app_state_catalog_overrides() {
    let llm_catalog_settings: LlmCatalogSettings = toml::from_str(
        r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"
priority = 120

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
    )
    .expect("catalog fixture should parse");
    let state = TestAppStateBuilder::new()
        .llm_catalog_settings(llm_catalog_settings)
        .build();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("GET")
        .uri(api("/models?provider=acme"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let models = body["data"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "acme-large");
    assert_eq!(models[0]["provider"], "acme");
}

#[tokio::test]
async fn list_providers_marks_configured_per_provider_and_omits_secrets() {
    // Only `ANTHROPIC_API_KEY` is supplied, so anthropic resolves as configured
    // while every other catalog provider does not.
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |name| (name == EnvVars::ANTHROPIC_API_KEY).then(|| "test-key".to_string()),
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("GET")
        .uri(api("/providers"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let providers = body["data"].as_array().unwrap();

    assert!(
        providers.len() >= 2,
        "builtin catalog should expose multiple providers"
    );

    let anthropic = providers
        .iter()
        .find(|provider| provider["id"] == "anthropic")
        .expect("anthropic provider should be present");
    assert_eq!(anthropic["configured"].as_bool(), Some(true));

    // `model_count` and `default_model` must reflect the catalog truth for
    // this exact provider, not merely be populated.
    let catalog = Catalog::builtin();
    let expected_model_count = catalog.list(Some(&ProviderId::anthropic())).len();
    assert_eq!(
        anthropic["model_count"].as_u64(),
        Some(expected_model_count as u64),
        "anthropic model_count should match the catalog"
    );
    let expected_default = catalog
        .default_for_provider(&ProviderId::anthropic())
        .expect("anthropic should have a catalog default model");
    assert_eq!(
        anthropic["default_model"].as_str(),
        Some(expected_default.id.as_str()),
        "anthropic default_model should match the catalog"
    );

    assert!(
        providers
            .iter()
            .filter(|provider| provider["id"] != "anthropic")
            .all(|provider| provider["configured"].as_bool() == Some(false)),
        "providers without supplied credentials should be unconfigured"
    );

    // Internal-only catalog fields and the injected credential value must
    // never reach the wire.
    let serialized = body["data"].to_string();
    assert!(!serialized.contains("\"auth\""), "leaked `auth`");
    assert!(
        !serialized.contains("\"extra_headers\""),
        "leaked `extra_headers`"
    );
    assert!(
        !serialized.contains("\"billing_policy\""),
        "leaked `billing_policy`"
    );
    assert!(
        !serialized.contains("\"agent_profile\""),
        "leaked `agent_profile`"
    );
    assert!(
        !serialized.contains("test-key"),
        "leaked the injected credential value"
    );
}

#[tokio::test]
async fn list_providers_marks_all_unconfigured_without_credentials() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("GET")
        .uri(api("/providers"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let providers = body["data"].as_array().unwrap();

    assert!(!providers.is_empty());
    assert!(
        providers
            .iter()
            .all(|provider| provider["configured"].as_bool() == Some(false)),
        "no provider should be configured when no credentials are supplied"
    );
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
    assert_eq!(run_json_id(&body).unwrap(), run_id);
    assert_eq!(body["goal"].as_str().unwrap(), "Test");
    assert_eq!(body["title"].as_str().unwrap(), "Test");
    assert!(body["repository"].is_object());
    assert!(!body["repository"]["name"].as_str().unwrap().is_empty());
    assert!(body["timestamps"]["created_at"].is_string());
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
    assert_eq!(run_json_id(&body), Some(run_id.as_str()));
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
    assert_eq!(run_json_id(&body), Some(newer_id.as_str()));
    assert_ne!(run_json_id(&body), Some(older_id.as_str()));
}

#[tokio::test]
async fn resolve_run_prefers_most_recent_collapsed_workflow_name_match() {
    let app = test_app_with();
    let older_id = create_run_for_target_with_workflow_name(
        &app,
        "nightly-alpha.fabro",
        &named_workflow_dot("OlderNightlyGraph", "older"),
        "Nightly_Build",
    )
    .await;
    let newer_id = create_run_for_target_with_workflow_name(
        &app,
        "nightly-beta.fabro",
        &named_workflow_dot("NewerNightlyGraph", "newer"),
        "Nightly_Build",
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
    assert_eq!(run_json_id(&body), Some(newer_id.as_str()));
    assert_ne!(run_json_id(&body), Some(older_id.as_str()));
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
            serde_json::to_string(&serde_json::json!({"kind": "yes"})).unwrap(),
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
                key:         "approve".to_string(),
                label:       "Approve".to_string(),
                description: None,
                preview:     None,
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

#[test]
fn validate_answer_for_question_accepts_no_for_confirmation() {
    let question = InterviewQuestionRecord {
        id:              "q-1".to_string(),
        text:            "Continue?".to_string(),
        stage:           "gate".to_string(),
        question_type:   QuestionType::Confirmation,
        options:         vec![],
        allow_freeform:  false,
        timeout_seconds: None,
        context_display: None,
    };

    let result = validate_answer_for_question(&question, &Answer::no());

    assert!(result.is_ok());
}

#[test]
fn answer_from_typed_yes_request_maps_to_yes_answer() {
    let question = InterviewQuestionRecord {
        id:              "q-1".to_string(),
        text:            "Continue?".to_string(),
        stage:           "gate".to_string(),
        question_type:   QuestionType::YesNo,
        options:         vec![],
        allow_freeform:  false,
        timeout_seconds: None,
        context_display: None,
    };
    let req: SubmitAnswerRequest = serde_json::from_value(json!({ "kind": "yes" })).unwrap();

    let answer = answer_from_request(req, &question).unwrap();

    assert_eq!(answer.value, AnswerValue::Yes);
}

#[test]
fn answer_from_typed_no_request_maps_to_no_answer() {
    let question = InterviewQuestionRecord {
        id:              "q-1".to_string(),
        text:            "Continue?".to_string(),
        stage:           "gate".to_string(),
        question_type:   QuestionType::YesNo,
        options:         vec![],
        allow_freeform:  false,
        timeout_seconds: None,
        context_display: None,
    };
    let req: SubmitAnswerRequest = serde_json::from_value(json!({ "kind": "no" })).unwrap();

    let answer = answer_from_request(req, &question).unwrap();

    assert_eq!(answer.value, AnswerValue::No);
}

#[test]
fn answer_from_typed_selected_request_validates_and_attaches_option() {
    let question = InterviewQuestionRecord {
        id:              "q-1".to_string(),
        text:            "Choose one.".to_string(),
        stage:           "gate".to_string(),
        question_type:   QuestionType::MultipleChoice,
        options:         vec![fabro_types::run_event::InterviewOption {
            key:         "approve".to_string(),
            label:       "Approve".to_string(),
            description: None,
            preview:     None,
        }],
        allow_freeform:  false,
        timeout_seconds: None,
        context_display: None,
    };
    let req: SubmitAnswerRequest =
        serde_json::from_value(json!({ "kind": "selected", "option_key": "approve" })).unwrap();

    let answer = answer_from_request(req, &question).unwrap();

    assert_eq!(answer.value, AnswerValue::Selected("approve".to_string()));
    assert_eq!(
        answer
            .selected_option
            .as_ref()
            .map(|option| option.label.as_str()),
        Some("Approve")
    );
}

#[test]
fn answer_from_typed_multi_selected_request_validates_option_keys() {
    let question = InterviewQuestionRecord {
        id:              "q-1".to_string(),
        text:            "Choose many.".to_string(),
        stage:           "gate".to_string(),
        question_type:   QuestionType::MultiSelect,
        options:         vec![
            fabro_types::run_event::InterviewOption {
                key:         "approve".to_string(),
                label:       "Approve".to_string(),
                description: None,
                preview:     None,
            },
            fabro_types::run_event::InterviewOption {
                key:         "notify".to_string(),
                label:       "Notify".to_string(),
                description: None,
                preview:     None,
            },
        ],
        allow_freeform:  false,
        timeout_seconds: None,
        context_display: None,
    };
    let req: SubmitAnswerRequest = serde_json::from_value(json!({
        "kind": "multi_selected",
        "option_keys": ["approve", "notify"],
    }))
    .unwrap();

    let answer = answer_from_request(req, &question).unwrap();

    assert_eq!(
        answer.value,
        AnswerValue::MultiSelected(vec!["approve".to_string(), "notify".to_string()])
    );
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
    let log_path = command_log_path(&run_dir, &stage_id);
    tokio::fs::create_dir_all(log_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&log_path, b"hello world").await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/logs/output?offset=6&limit=5"
        )))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let bytes = BASE64_STANDARD
        .decode(body["bytes_base64"].as_str().unwrap())
        .unwrap();

    assert!(body.get("stream").is_none());
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
    append_default_run_created(&run_store, run_id).await;
    let output_blob = run_store
        .write_blob(&serde_json::to_vec("hello world").unwrap())
        .await
        .unwrap();
    let output_ref = format!("blob://sha256/{output_blob}");
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
            node_id:        "script_node".to_string(),
            output:         output_ref.clone(),
            exit_code:      Some(0),
            duration_ms:    5,
            termination:    CommandTermination::Exited,
            output_bytes:   11,
            live_streaming: false,
        },
    ] {
        workflow_event::append_event(&run_store, &run_id, &event)
            .await
            .unwrap();
    }

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/script_node@1/logs/output?offset=6&limit=5"
        )))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let bytes = BASE64_STANDARD
        .decode(body["bytes_base64"].as_str().unwrap())
        .unwrap();

    assert!(body.get("stream").is_none());
    assert_eq!(body["offset"], 6);
    assert_eq!(body["next_offset"], 11);
    assert_eq!(body["total_bytes"], 11);
    assert_eq!(bytes, b"world");
    assert_eq!(body["eof"], true);
    assert_eq!(body["cas_ref"], output_ref);
    assert_eq!(body["live_streaming"], false);
}

#[tokio::test]
async fn get_run_stage_command_log_prefers_scratch_when_cas_ref_exists() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let stage_id = StageId::new("script_node", 1);
    let run_store = state.store.create_run(&run_id).await.unwrap();
    append_default_run_created(&run_store, run_id).await;
    let output_blob = run_store
        .write_blob(&serde_json::to_vec("cas log").unwrap())
        .await
        .unwrap();
    let output_ref = format!("blob://sha256/{output_blob}");
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
            node_id:        "script_node".to_string(),
            output:         output_ref.clone(),
            exit_code:      Some(0),
            duration_ms:    5,
            termination:    CommandTermination::Exited,
            output_bytes:   7,
            live_streaming: false,
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
    let log_path = command_log_path(&run_dir, &stage_id);
    tokio::fs::create_dir_all(log_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&log_path, b"scratch log").await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/stages/{stage_id}/logs/output?offset=0&limit=64"
        )))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let bytes = BASE64_STANDARD
        .decode(body["bytes_base64"].as_str().unwrap())
        .unwrap();

    assert!(body.get("stream").is_none());
    assert_eq!(body["offset"], 0);
    assert_eq!(body["next_offset"], 11);
    assert_eq!(body["total_bytes"], 11);
    assert_eq!(bytes, b"scratch log");
    assert_eq!(body["eof"], true);
    assert_eq!(body["cas_ref"], output_ref);
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
        .uri(api(&format!("/runs/{run_id}/stages/missing@1/logs/output")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_run_stage_context_window_returns_not_found_for_missing_run() {
    let app = crate::test_support::build_test_router(test_app_state_with_isolated_storage());
    let run_id = RunId::new();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!(
                    "/runs/{run_id}/stages/agent@1/context-window"
                )))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_run_stage_context_window_returns_not_found_for_missing_stage() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[workflow_event::Event::RunSubmitted {
        definition_blob: None,
    }])
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!(
                    "/runs/{run_id}/stages/missing@1/context-window"
                )))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn get_run_stage_context_window_returns_unavailable_for_non_agent_stage() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        stage_started_event("script_node", "command"),
        command_started_event("script_node"),
    ])
    .await;

    let body = response_json!(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!(
                    "/runs/{run_id}/stages/script_node@1/context-window"
                )))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
        StatusCode::OK
    )
    .await;

    assert_eq!(body["available"], false);
    assert_eq!(body["unavailable_reason"], "not_agent_stage");
    assert_eq!(body["breakdown"], json!([]));
    assert_eq!(body["staleness"], "unavailable");
}

#[tokio::test]
async fn get_run_stage_context_window_returns_not_observed_for_agent_stage_without_snapshot() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        agent_session_activated_event("agent_node", 1),
    ])
    .await;

    let body = response_json!(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!(
                    "/runs/{run_id}/stages/agent_node@1/context-window"
                )))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
        StatusCode::OK
    )
    .await;

    assert_eq!(body["available"], false);
    assert_eq!(body["unavailable_reason"], "not_observed");
    assert_eq!(body["input_tokens"], serde_json::Value::Null);
    assert!(!body["warnings"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn get_run_stage_context_window_returns_live_projected_snapshot() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        stage_started_event("agent_node", "agent"),
        context_window_event(
            "agent_node",
            1,
            context_window_snapshot(123_456, Vec::new()),
        ),
    ])
    .await;

    let body = response_json!(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!(
                    "/runs/{run_id}/stages/agent_node@1/context-window"
                )))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
        StatusCode::OK
    )
    .await;

    assert_eq!(body["stage_id"], "agent_node@1");
    assert_eq!(body["available"], true);
    assert_eq!(body["provider"], "openai");
    assert_eq!(body["count_method"], "provider_api_scaled_breakdown");
    assert_eq!(body["staleness"], "live");
    assert_eq!(body["input_tokens"], 123_456);
    assert_eq!(body["breakdown"][0]["category"], "conversation");
}

#[tokio::test]
async fn get_run_stage_context_window_marks_completed_stage_snapshot_stored() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        stage_started_event("agent_node", "agent"),
        context_window_event("agent_node", 1, context_window_snapshot(100, Vec::new())),
        stage_completed_event("agent_node"),
    ])
    .await;

    let body = response_json!(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!(
                    "/runs/{run_id}/stages/agent_node@1/context-window"
                )))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
        StatusCode::OK
    )
    .await;

    assert_eq!(body["available"], true);
    assert_eq!(body["staleness"], "stored");
    assert_eq!(body["input_tokens"], 100);
}

#[tokio::test]
async fn get_run_stage_context_window_returns_projected_warnings() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        stage_started_event("agent_node", "agent"),
        context_window_event(
            "agent_node",
            1,
            context_window_snapshot(100, vec![StageContextWindowWarning {
                code:    "provider_token_count_failed".to_string(),
                message: "provider input token counting failed; returned local estimate"
                    .to_string(),
            }]),
        ),
    ])
    .await;

    let body = response_json!(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!(
                    "/runs/{run_id}/stages/agent_node@1/context-window"
                )))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
        StatusCode::OK
    )
    .await;

    assert_eq!(body["warnings"][0]["code"], "provider_token_count_failed");
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

    assert_eq!(body["data"]["link"]["number"], 42);
    assert_eq!(body["data"]["link"]["owner"], "acme");
    assert_eq!(body["data"]["details"]["state"], "closed");
    assert_eq!(body["data"]["details"]["merged"], true);
    assert_eq!(body["data"]["details"]["head_branch"], "feature");
    assert_eq!(body["data"]["details"]["base_branch"], "main");
    assert_eq!(body["meta"]["details_status"], "available");
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
async fn link_run_pull_request_links_github_pr_from_any_repo_and_updates_state() {
    let (_state, app, run_id) = pr_test_app_with_minimal_run(None, None).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "html_url": "https://github.com/other/repo/pull/987"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["html_url"], "https://github.com/other/repo/pull/987");
    assert_eq!(body["owner"], "other");
    assert_eq!(body["repo"], "repo");
    assert_eq!(body["number"], 987);

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
    assert_eq!(
        state_body["pull_request"]["html_url"],
        "https://github.com/other/repo/pull/987"
    );
}

#[tokio::test]
async fn link_run_pull_request_rejects_non_github_url() {
    let (_state, app, run_id) = pr_test_app_with_minimal_run(None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "html_url": "https://gitlab.com/acme/widgets/-/merge_requests/42"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;

    assert_eq!(
        body["errors"][0]["code"],
        "unsupported_pull_request_provider"
    );
}

#[tokio::test]
async fn unlink_run_pull_request_appends_event_and_clears_projected_state() {
    let (state, app, run_id) = pr_test_app_with_minimal_run(None, None).await;
    let link_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "html_url": "https://github.com/acme/widgets/pull/42"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    response_json!(link_response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(api(&format!("/runs/{run_id}/pull_request")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

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
    assert!(state_body["pull_request"].is_null());

    let run_id = run_id.parse::<RunId>().unwrap();
    let run_store = state.store.open_run_reader(&run_id).await.unwrap();
    let events = run_store.list_events().await.unwrap();
    assert!(events.iter().any(|event| {
        event.event.event_name() == "pull_request.unlinked"
            && event.event.properties().unwrap()["pull_request"]["html_url"]
                == "https://github.com/acme/widgets/pull/42"
    }));
}

#[tokio::test]
async fn get_run_pull_request_returns_stored_github_association_without_github_credentials() {
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
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["data"]["link"]["number"], 42);
    assert_eq!(
        body["data"]["link"]["html_url"],
        "https://github.com/acme/widgets/pull/42"
    );
    assert!(body["data"]["details"].is_null());
    assert_eq!(body["meta"]["details_status"], "unavailable");
    assert_eq!(
        body["meta"]["details_unavailable_reason"],
        "integration_unavailable"
    );
}

#[tokio::test]
async fn get_run_pull_request_returns_stored_github_association_when_github_pr_is_missing() {
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
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["data"]["link"]["number"], 42);
    assert_eq!(
        body["data"]["link"]["html_url"],
        "https://github.com/acme/widgets/pull/42"
    );
    assert!(body["data"]["details"].is_null());
    assert_eq!(body["meta"]["details_status"], "unavailable");
    assert_eq!(body["meta"]["details_unavailable_reason"], "not_found");
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
                .json_body(openai_responses_payload(
                    &serde_json::to_string(&json!({
                        "title": "Mock title",
                        "body": "Narrative from mock.",
                    }))
                    .unwrap(),
                ));
        })
        .await;
    let state = create_github_token_app_state_with_env_lookup_and_llm_catalog_settings(
        Some("ghu_test"),
        Some(github.base_url()),
        |_| None,
        llm_catalog_settings_with_provider_base_url("openai", llm.url("/v1")),
    );
    state
        .vault
        .write()
        .await
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    Box::pin(create_completed_run_ready_for_pull_request(
        &state,
        run_id,
        Some("git@github.com:acme/widgets.git"),
        Some("main"),
        Some("fabro/run/42"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+fn shipped() {}\n",
    ))
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
    assert_eq!(state_body["pull_request"]["owner"], "acme");
    assert_eq!(state_body["pull_request"]["repo"], "widgets");

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
    let (_state, app, run_id) = Box::pin(pr_test_app_with_completed_run(None, None, None)).await;

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
    let (_state, app, run_id) = Box::pin(pr_test_app_with_completed_run(
        None,
        None,
        Some("https://github.com/acme/widgets.git"),
    ))
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
    let (_state, app, run_id) = Box::pin(pr_test_app_with_completed_run(
        Some("ghu_test"),
        None,
        Some("https://gitlab.com/acme/widgets.git"),
    ))
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
async fn merge_run_pull_request_uses_stored_link_coordinates() {
    let github = MockServer::start();
    let github_mock = github.mock(|when, then| {
        when.method("PUT")
            .path("/repos/acme/widgets/pulls/42/merge")
            .header("authorization", "Bearer ghu_test")
            .json_body(json!({ "merge_method": "squash" }));
        then.status(200)
            .header("content-type", "application/json")
            .body(json!({}).to_string());
    });
    let (state, app, run_id) = pr_test_app(Some("ghu_test"), Some(github.base_url()));

    create_run_with_linked_pull_request_record(&state, run_id, PullRequestLink {
        owner:  "acme".to_string(),
        repo:   "widgets".to_string(),
        number: 42,
    })
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
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["number"], 42);
    assert_eq!(body["html_url"], "https://github.com/acme/widgets/pull/42");
    github_mock.assert();
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
async fn cache_backed_run_endpoints_reflect_events_appended_after_warmup() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();

    state.store.warm_projection_cache().await.unwrap();

    let run_store = state.store.open_run(&run_id).await.unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunnable {
        source: fabro_types::RunRunnableSource::StartRequested,
        actor:  None,
    })
    .await
    .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunStarting)
        .await
        .unwrap();
    workflow_event::append_event(&run_store, &run_id, &workflow_event::Event::RunRunning)
        .await
        .unwrap();
    append_scoped_stage_event(
        &state,
        run_id,
        "review",
        1,
        &workflow_event::Event::StageStarted {
            node_id:      "review".to_string(),
            name:         "Review".to_string(),
            index:        0,
            handler_type: "human".to_string(),
            attempt:      1,
            max_attempts: 1,
        },
    )
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "cache-question",
        "2026-04-19T12:00:00Z",
        "interview.started",
        json!({
            "question_id": "q-cache",
            "question": "Approve cached deploy?",
            "stage": "review",
            "question_type": "yes_no",
            "options": [],
            "allow_freeform": false,
            "context_display": null,
            "timeout_seconds": null,
        }),
        Some("review"),
    )
    .await;
    append_raw_run_event(
        &state,
        run_id,
        "cache-checkpoint",
        "2026-04-19T12:00:01Z",
        "checkpoint.completed",
        json!({
            "status": "running",
            "current_node": "review",
            "completed_nodes": [],
            "node_retries": {},
            "context_values": {},
            "node_outcomes": {},
            "next_node_id": "review",
            "git_commit_sha": "cache-sha",
            "loop_failure_signatures": {},
            "restart_failure_signatures": {},
            "node_visits": { "review": 1 },
        }),
        Some("review"),
    )
    .await;

    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response_json!(status, StatusCode::OK).await;
    assert_eq!(run_json_status(&status)["kind"].as_str(), Some("running"));

    let state_response = app
        .clone()
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
    assert_eq!(
        state_body["pending_interviews"]["q-cache"]["question"]["text"].as_str(),
        Some("Approve cached deploy?")
    );

    let stages = app
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
    let stages = response_json!(stages, StatusCode::OK).await;
    assert_eq!(stages["data"][0]["id"].as_str(), Some("review@1"));

    let questions = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/questions")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let questions = response_json!(questions, StatusCode::OK).await;
    assert_eq!(
        questions["data"][0]["text"].as_str(),
        Some("Approve cached deploy?")
    );

    let settings = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/settings")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(settings, StatusCode::OK).await;

    let checkpoint = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/checkpoint")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let checkpoint = response_json!(checkpoint, StatusCode::OK).await;
    assert_eq!(checkpoint["git_commit_sha"].as_str(), Some("cache-sha"));

    let billing = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}/billing")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let billing = response_json!(billing, StatusCode::OK).await;
    assert_eq!(billing["stages"][0]["stage"]["id"].as_str(), Some("review"));
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

    assert_eq!(run_state.spec.graph.name, "Test");
}

#[tokio::test]
async fn create_run_keeps_missing_project_and_workflow_names_absent() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let manifest = serde_json::json!({
        "version": 1,
        "cwd": "/tmp/project",
        "target": {
            "identifier": "workflow.fabro",
            "path": "workflow.fabro",
        },
        "configs": [
            {
                "path": "/tmp/project/.fabro/project.toml",
                "source": "_version = 1\n",
                "type": "project",
            }
        ],
        "workflows": {
            "workflow.fabro": {
                "source": "digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
                "config": {
                    "path": "workflow.toml",
                    "source": "_version = 1\n",
                },
                "files": {},
            }
        },
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(manifest.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let run_state = state
        .store
        .open_run_reader(&run_id)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();

    assert_eq!(run_state.spec.settings.project.name.as_deref(), None);
    assert_eq!(run_state.spec.settings.workflow.name.as_deref(), None);
    assert_eq!(run_state.spec.graph_name(), Some("Demo"));
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

    for path in [
        format!("/runs/{run_id}"),
        format!("/runs/{run_id}/questions"),
    ] {
        let response = app
            .clone()
            .oneshot(bearer_request(
                Method::GET,
                &path,
                &worker_token,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_status!(response, StatusCode::OK).await;
    }

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
async fn run_tool_worker_token_can_use_client_backend_routes_across_runs() {
    let (state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let parent_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let target_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let run_tool_worker_token = issue_test_run_tools_worker_token(&parent_run_id);

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            "/runs",
            &run_tool_worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/resolve?selector={target_run_id}"),
            &run_tool_worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    for path in [
        format!("/runs/{target_run_id}"),
        format!("/runs/{target_run_id}/state"),
        format!("/runs/{target_run_id}/events"),
        format!("/runs/{target_run_id}/questions"),
    ] {
        let response = app
            .clone()
            .oneshot(bearer_request(
                Method::GET,
                &path,
                &run_tool_worker_token,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_status!(response, StatusCode::OK).await;
    }

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            &format!("/runs/{target_run_id}/start"),
            &run_tool_worker_token,
            &json!({ "resume": false }),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/runs/{target_run_id}/cancel"),
            &run_tool_worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    for path in [
        format!("/runs/{target_run_id}/archive"),
        format!("/runs/{target_run_id}/unarchive"),
        format!("/runs/{target_run_id}/interrupt"),
    ] {
        let response = app
            .clone()
            .oneshot(bearer_request(
                Method::POST,
                &path,
                &run_tool_worker_token,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_ne!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            &format!("/runs/{target_run_id}/steer"),
            &run_tool_worker_token,
            &json!({ "text": "continue", "interrupt": false }),
        ))
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(response.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            &format!("/runs/{target_run_id}/questions/q-1/answer"),
            &run_tool_worker_token,
            &json!({ "kind": "yes" }),
        ))
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(response.status(), StatusCode::FORBIDDEN);

    let created_child = create_run_with_bearer(&app, &run_tool_worker_token).await;
    let cached = state
        .store
        .get_cached_run(&created_child)
        .await
        .unwrap()
        .expect("created run should be cached");
    assert_eq!(
        cached
            .projection
            .spec
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.subject.as_ref()),
        Some(&Principal::Worker {
            run_id: parent_run_id,
        }),
    );

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::PUT,
            &format!("/runs/{created_child}/parent"),
            &run_tool_worker_token,
            &json!({ "parent_id": target_run_id.to_string() }),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::DELETE,
            &format!("/runs/{created_child}/parent"),
            &run_tool_worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;
}

#[tokio::test]
async fn base_worker_token_is_rejected_by_run_tool_only_routes() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);

    for (method, path) in [
        (Method::GET, "/runs".to_string()),
        (Method::POST, "/runs".to_string()),
        (Method::GET, "/runs/resolve?selector=latest".to_string()),
    ] {
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
            "{method} {path} unexpectedly accepted base worker token with status {}",
            response.status()
        );
    }
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
            &format!("/runs/{run_id}/stages/code@1/logs/output"),
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
            &format!("/runs/{run_id}/stages/code@1/logs/output"),
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
            &format!("/runs/{run_id}/stages/code@1/logs/output"),
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
                .uri(api(&format!("/runs/{run_id}/stages/code@1/logs/output")))
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
        (Method::DELETE, format!("/runs/{run_id}")),
        (Method::GET, format!("/runs/{run_id}/attach")),
        (Method::GET, format!("/runs/{run_id}/checkpoint")),
        (Method::POST, format!("/runs/{run_id}/pause")),
        (Method::POST, format!("/runs/{run_id}/unpause")),
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
        (Method::GET, format!("/runs/{run_id}/sandbox/services")),
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
    assert_eq!(run_json_status(&body)["kind"], "submitted");
    assert_eq!(body["title"], "Test");
}

#[tokio::test]
async fn create_run_accepts_explicit_title() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let mut manifest = minimal_manifest_json(MINIMAL_DOT);
    manifest["title"] = json!("  Explicit server title  ");

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&manifest).unwrap()))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    assert_eq!(body["title"], "Explicit server title");

    let run_id = body["id"].as_str().unwrap();
    let detail_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{run_id}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail = response_json!(detail_response, StatusCode::OK).await;
    assert_eq!(detail["title"], "Explicit server title");
}

#[tokio::test]
async fn create_run_rejects_invalid_titles() {
    let app = test_app_with();
    for title in [
        "   ".to_string(),
        "First\nSecond".to_string(),
        "x".repeat(101),
    ] {
        let mut manifest = minimal_manifest_json(MINIMAL_DOT);
        manifest["title"] = json!(title);
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&manifest).unwrap()))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_status!(response, StatusCode::BAD_REQUEST).await;
    }
}

#[tokio::test]
async fn start_run_transitions_to_runnable() {
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
    assert_eq!(run_json_status(&body)["kind"], "runnable");
    assert_eq!(body["title"], "Test");

    let status = state
        .store
        .open_run_reader(&run_id.parse::<RunId>().unwrap())
        .await
        .unwrap()
        .state()
        .await
        .unwrap()
        .status;
    assert_eq!(status, RunStatus::Runnable);
}

#[tokio::test]
async fn worker_started_child_run_requires_approval_before_becoming_runnable() {
    let (state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let parent_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_run_tools_worker_token(&parent_run_id);
    let mut child_manifest = minimal_manifest_json(MINIMAL_DOT);
    child_manifest["parent_id"] = json!(parent_run_id.to_string());

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &worker_token,
            &child_manifest,
        ))
        .await
        .unwrap();
    let child_body = response_json!(response, StatusCode::CREATED).await;
    let child_run_id = child_body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            &format!("/runs/{child_run_id}/start"),
            &worker_token,
            &json!({ "resume": false }),
        ))
        .await
        .unwrap();
    let pending_body = response_json!(response, StatusCode::OK).await;
    assert_eq!(
        run_json_status(&pending_body),
        &json!({
            "kind": "pending",
            "reason": "approval_required"
        })
    );
    assert_eq!(
        pending_body["lifecycle"]["approval"]["state"].as_str(),
        Some("pending")
    );

    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        assert_eq!(
            runs.get(&child_run_id).map(|run| run.status),
            Some(RunStatus::Pending {
                reason: fabro_types::PendingReason::ApprovalRequired,
            })
        );
    }

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/runs/{child_run_id}/approve"),
            &user_jwt,
            Body::empty(),
        ))
        .await
        .unwrap();
    let approved_body = response_json!(response, StatusCode::OK).await;
    assert_eq!(
        run_json_status(&approved_body),
        &json!({ "kind": "runnable" })
    );
    assert_eq!(
        approved_body["lifecycle"]["approval"]["state"].as_str(),
        Some("approved")
    );
    assert!(
        approved_body["lifecycle"]["approval"]["decided_at"]
            .as_str()
            .is_some()
    );

    let runs = state.runs.lock().expect("runs lock poisoned");
    assert_eq!(
        runs.get(&child_run_id).map(|run| run.status),
        Some(RunStatus::Runnable)
    );
}

#[tokio::test]
async fn denying_pending_child_run_fails_with_approval_denied() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let parent_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_run_tools_worker_token(&parent_run_id);
    let mut child_manifest = minimal_manifest_json(MINIMAL_DOT);
    child_manifest["parent_id"] = json!(parent_run_id.to_string());

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &worker_token,
            &child_manifest,
        ))
        .await
        .unwrap();
    let child_body = response_json!(response, StatusCode::CREATED).await;
    let child_run_id = child_body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            &format!("/runs/{child_run_id}/start"),
            &worker_token,
            &json!({ "resume": false }),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            &format!("/runs/{child_run_id}/deny"),
            &user_jwt,
            &json!({ "reason": "  " }),
        ))
        .await
        .unwrap();
    let denied_body = response_json!(response, StatusCode::OK).await;
    assert_eq!(
        run_json_status(&denied_body),
        &json!({
            "kind": "failed",
            "reason": "approval_denied"
        })
    );
    assert_eq!(
        denied_body["lifecycle"]["approval"]["state"].as_str(),
        Some("denied")
    );
    assert!(denied_body["lifecycle"]["approval"]["denial_reason"].is_null());
}

#[tokio::test]
async fn patch_run_title_updates_active_and_archived_runs() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();

    let patch_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(api(&format!("/runs/{run_id}")))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "title": "  Active title  " }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let patch_body = response_json!(patch_response, StatusCode::OK).await;
    assert_eq!(patch_body["title"], "Active title");

    let run_store = state.store.open_run_reader(&run_id).await.unwrap();
    let event_count = run_store.list_events().await.unwrap().len();
    let same_title_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(api(&format!("/runs/{run_id}")))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "title": "Active title" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let same_title_body = response_json!(same_title_response, StatusCode::OK).await;
    assert_eq!(same_title_body["title"], "Active title");
    assert_eq!(
        state
            .store
            .open_run_reader(&run_id)
            .await
            .unwrap()
            .list_events()
            .await
            .unwrap()
            .len(),
        event_count,
        "same-title PATCH should not append an event"
    );

    let run_store = state.store.open_run(&run_id).await.unwrap();
    for event in [
        workflow_event::Event::RunRunnable {
            source: fabro_types::RunRunnableSource::StartRequested,
            actor:  None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ] {
        workflow_event::append_event(&run_store, &run_id, &event)
            .await
            .unwrap();
    }
    workflow_event::append_event(
        &run_store,
        &run_id,
        &workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    )
    .await
    .unwrap();
    response_json!(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(api(&format!("/runs/{run_id}/archive")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::OK
    )
    .await;

    let archived_patch_response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(api(&format!("/runs/{run_id}")))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "title": "Archived title" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let archived_patch_body = response_json!(archived_patch_response, StatusCode::OK).await;
    assert_eq!(archived_patch_body["title"], "Archived title");
    assert!(run_json_archived(&archived_patch_body));
}

#[tokio::test]
async fn patch_run_title_rejects_invalid_titles() {
    let app = test_app_with();
    let run_id = create_run(&app, MINIMAL_DOT).await;

    for title in [String::new(), "Bad\rTitle".to_string(), "x".repeat(101)] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(api(&format!("/runs/{run_id}")))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "title": title }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_status!(response, StatusCode::BAD_REQUEST).await;
    }
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

    // Start it (transitions to runnable)
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
async fn retry_failed_run_creates_and_queues_new_run() {
    let state = test_app_state_with_isolated_storage();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let source_run_id = RunId::new();
    create_durable_run_with_events(&state, source_run_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::workflow_run_failed_from_error(
            &WorkflowError::engine("boom"),
            fabro_types::RunTiming::wall_only(10),
            FailureReason::WorkflowError,
            None,
            None,
            None,
            None,
        ),
    ])
    .await;
    let source_events_before = state
        .store
        .open_run(&source_run_id)
        .await
        .unwrap()
        .list_events()
        .await
        .unwrap()
        .len();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{source_run_id}/retry")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::CREATED).await;
    let new_run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    assert_ne!(new_run_id, source_run_id);
    assert_eq!(body["retried_from"], source_run_id.to_string());
    assert_eq!(body["created_by"]["kind"], "user");
    assert_eq!(body["created_by"]["login"], "dev");
    assert_eq!(run_json_status(&body)["kind"], "runnable");

    let source_store = state.store.open_run(&source_run_id).await.unwrap();
    assert_eq!(
        source_store.list_events().await.unwrap().len(),
        source_events_before
    );
    assert_eq!(
        source_store.state().await.unwrap().status,
        RunStatus::Failed {
            reason: FailureReason::WorkflowError,
        }
    );

    let new_state = state
        .store
        .open_run(&new_run_id)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(new_state.retried_from, Some(source_run_id));
    assert_eq!(new_state.status, RunStatus::Runnable);
    assert!(new_state.checkpoints.is_empty());
}

#[tokio::test]
async fn retry_missing_run_returns_not_found() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{}/retry", fixtures::RUN_64)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn retry_non_retryable_run_returns_conflict() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let source_run_id = RunId::new();
    create_durable_run_with_events(&state, source_run_id, &[
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(10),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{source_run_id}/retry")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

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
async fn cancel_terminal_durable_run_returns_conflict() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CONFLICT).await;
    assert_eq!(
        body["errors"][0]["detail"],
        "Run is already terminal and cannot be cancelled."
    );
}

#[tokio::test]
async fn steer_nonexistent_run_returns_not_found() {
    let app = test_app_with();
    let missing_run_id = fixtures::RUN_64;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{missing_run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn steer_terminal_durable_run_returns_run_not_steerable() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::CONFLICT).await;
    assert_eq!(body["errors"][0]["code"], "run_not_steerable");
    assert_eq!(body["errors"][0]["detail"], "Run is no longer steerable.");
}

#[tokio::test]
async fn steer_empty_text_returns_bad_request() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_and_start_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"   "}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    // 400 (whitespace-only text) or 409 (run not yet `running` when the
    // handler checks status) are both acceptable; the only outcome we
    // want to rule out is a successful enqueue.
    let status = response.status();
    assert!(
        matches!(status, StatusCode::BAD_REQUEST | StatusCode::CONFLICT),
        "expected 400 or 409, got {status}"
    );
}

fn insert_running_control_run(
    state: &Arc<AppState>,
    run_id: RunId,
    answer_transport: Option<RunAnswerTransport>,
) -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut run = managed_run(
        String::new(),
        RunStatus::Running,
        chrono::Utc::now(),
        temp_dir.path().join(run_id.to_string()),
        RunExecutionMode::Start,
    );
    run.answer_transport = answer_transport;
    state
        .runs
        .lock()
        .expect("runs lock poisoned")
        .insert(run_id, run);
    temp_dir
}

#[tokio::test]
async fn steer_without_active_steerable_session_forwards_plain_steer_for_buffering() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let _temp_dir = insert_running_control_run(
        &state,
        run_id,
        Some(RunAnswerTransport::Subprocess { control_tx }),
    );

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = control_rx.recv().await.unwrap();
    assert!(matches!(
        envelope.message,
        WorkerControlMessage::Steer { ref text, .. } if text == "try again"
    ));
}

#[tokio::test]
async fn steer_with_active_non_steerable_session_returns_conflict() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let stage_id = StageId::new("agent", 1);
    let (control_tx, _control_rx) = tokio::sync::mpsc::channel(1);
    let _temp_dir = insert_running_control_run(
        &state,
        run_id,
        Some(RunAnswerTransport::Subprocess { control_tx }),
    );
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.get_mut(&run_id)
            .unwrap()
            .active_non_steerable_stages
            .insert(stage_id, "session-a".to_string());
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["errors"][0]["code"], "agent_not_steerable");
}

#[tokio::test]
async fn steer_interrupt_without_active_steerable_session_returns_conflict() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (control_tx, _control_rx) = tokio::sync::mpsc::channel(1);
    let _temp_dir = insert_running_control_run(
        &state,
        run_id,
        Some(RunAnswerTransport::Subprocess { control_tx }),
    );

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again","interrupt":true}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["errors"][0]["code"], "no_active_steerable_session");
}

#[tokio::test]
async fn interrupt_with_active_steerable_session_forwards_interrupt() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let stage_id = StageId::new("agent", 1);
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let _temp_dir = insert_running_control_run(
        &state,
        run_id,
        Some(RunAnswerTransport::Subprocess { control_tx }),
    );
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.get_mut(&run_id)
            .unwrap()
            .active_steerable_stages
            .insert(stage_id, "session-a".to_string());
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = control_rx.recv().await.unwrap();
    assert!(matches!(
        envelope.message,
        WorkerControlMessage::Interrupt {
            actor: Principal::User(_),
        }
    ));
}

#[tokio::test]
async fn steer_interrupt_with_active_steerable_session_forwards_combined_control_message() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let stage_id = StageId::new("agent", 1);
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let _temp_dir = insert_running_control_run(
        &state,
        run_id,
        Some(RunAnswerTransport::Subprocess { control_tx }),
    );
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.get_mut(&run_id)
            .unwrap()
            .active_steerable_stages
            .insert(stage_id, "session-a".to_string());
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again","interrupt":true}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = control_rx.recv().await.unwrap();
    assert!(matches!(
        envelope.message,
        WorkerControlMessage::InterruptThenSteer { ref text, .. } if text == "try again"
    ));
}

#[tokio::test]
async fn interrupt_terminal_run_returns_run_not_interruptible() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let temp_dir = tempfile::tempdir().unwrap();
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            run_id,
            managed_run(
                String::new(),
                RunStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
                chrono::Utc::now(),
                temp_dir.path().join(run_id.to_string()),
                RunExecutionMode::Start,
            ),
        );
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["errors"][0]["code"], "run_not_interruptible");
}

#[test]
fn injected_runnable_event_does_not_make_submitted_run_schedulable() {
    let state = test_app_state();
    let run_id = fixtures::RUN_1;
    let temp_dir = tempfile::tempdir().unwrap();
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            run_id,
            managed_run(
                String::new(),
                RunStatus::Submitted,
                chrono::Utc::now(),
                temp_dir.path().join(run_id.to_string()),
                RunExecutionMode::Start,
            ),
        );
    }

    let runnable = workflow_event::to_run_event(&run_id, &workflow_event::Event::RunRunnable {
        source: fabro_types::RunRunnableSource::StartRequested,
        actor:  None,
    });
    update_live_run_from_event(&state, run_id, &runnable);

    {
        let runs = state.runs.lock().expect("runs lock poisoned");
        assert_eq!(runs.get(&run_id).unwrap().status, RunStatus::Submitted);
    }

    let starting = workflow_event::to_run_event(&run_id, &workflow_event::Event::RunStarting);
    update_live_run_from_event(&state, run_id, &starting);

    let runs = state.runs.lock().expect("runs lock poisoned");
    assert_eq!(runs.get(&run_id).unwrap().status, RunStatus::Starting);
}

#[test]
fn active_steerable_stage_projection_ignores_stale_deactivation() {
    let state = test_app_state();
    let run_id = fixtures::RUN_1;
    let temp_dir = tempfile::tempdir().unwrap();
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.insert(
            run_id,
            managed_run(
                String::new(),
                RunStatus::Running,
                chrono::Utc::now(),
                temp_dir.path().join(run_id.to_string()),
                RunExecutionMode::Start,
            ),
        );
    }

    let stage_id = StageId::new("agent", 1);
    let activated_a =
        workflow_event::to_run_event(&run_id, &workflow_event::Event::AgentSessionActivated {
            node_id:          "agent".to_string(),
            visit:            1,
            session_id:       "session-a".to_string(),
            thread_id:        None,
            provider:         Some("openai".to_string()),
            model:            Some("gpt-5.4".to_string()),
            reasoning_effort: None,
            speed:            None,
            permission_level: None,
            capabilities:     vec![SessionCapability::Steer],
        });
    update_live_run_from_event(&state, run_id, &activated_a);

    let deactivated_a =
        workflow_event::to_run_event(&run_id, &workflow_event::Event::AgentSessionDeactivated {
            node_id:    "agent".to_string(),
            visit:      1,
            session_id: "session-a".to_string(),
        });
    update_live_run_from_event(&state, run_id, &deactivated_a);

    let activated_b =
        workflow_event::to_run_event(&run_id, &workflow_event::Event::AgentSessionActivated {
            node_id:          "agent".to_string(),
            visit:            1,
            session_id:       "session-b".to_string(),
            thread_id:        None,
            provider:         Some("openai".to_string()),
            model:            Some("gpt-5.4".to_string()),
            reasoning_effort: None,
            speed:            None,
            permission_level: None,
            capabilities:     vec![SessionCapability::Steer],
        });
    update_live_run_from_event(&state, run_id, &activated_b);
    update_live_run_from_event(&state, run_id, &deactivated_a);

    let runs = state.runs.lock().expect("runs lock poisoned");
    let run = runs.get(&run_id).unwrap();
    assert_eq!(
        run.active_steerable_stages
            .get(&stage_id)
            .map(String::as_str),
        Some("session-b")
    );
}

fn acp_event_for_stage(run_id: &RunId, event: &workflow_event::Event) -> fabro_types::RunEvent {
    workflow_event::to_run_event_at(
        run_id,
        event,
        Utc::now(),
        Some(&workflow_event::StageScope {
            node_id:            "agent".to_string(),
            visit:              1,
            parallel_group_id:  None,
            parallel_branch_id: None,
        }),
    )
}

#[tokio::test]
async fn steer_with_active_acp_session_forwards_to_worker() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(1);
    let _temp_dir = insert_running_control_run(
        &state,
        run_id,
        Some(RunAnswerTransport::Subprocess { control_tx }),
    );

    let started = acp_event_for_stage(&run_id, &workflow_event::Event::AgentAcpStarted {
        node_id:     "agent".to_string(),
        visit:       1,
        command:     "python fake_agent.py".to_string(),
        config_name: None,
    });
    update_live_run_from_event(&state, run_id, &started);
    let activated =
        workflow_event::to_run_event(&run_id, &workflow_event::Event::AgentSessionActivated {
            node_id:          "agent".to_string(),
            visit:            1,
            session_id:       "acp-session".to_string(),
            thread_id:        None,
            provider:         Some(AgentBackend::Acp.to_string()),
            model:            None,
            reasoning_effort: None,
            speed:            None,
            permission_level: None,
            capabilities:     vec![SessionCapability::Steer],
        });
    update_live_run_from_event(&state, run_id, &activated);

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = control_rx.recv().await.unwrap();
    assert!(matches!(
        envelope.message,
        WorkerControlMessage::Steer { ref text, .. } if text == "try again"
    ));
}

#[tokio::test]
async fn active_acp_steerable_marker_clears_on_terminal_paths() {
    let terminal_events: Vec<workflow_event::Event> = vec![
        workflow_event::Event::AgentAcpCompleted {
            node_id:     "agent".to_string(),
            stdout:      "done".to_string(),
            stderr:      String::new(),
            stop_reason: "end_turn".to_string(),
            duration_ms: 42,
        },
        workflow_event::Event::AgentAcpCancelled {
            node_id:     "agent".to_string(),
            stdout:      "partial".to_string(),
            stderr:      "cancelled".to_string(),
            duration_ms: 7,
        },
        workflow_event::Event::AgentAcpTimedOut {
            node_id:     "agent".to_string(),
            stdout:      "partial".to_string(),
            stderr:      "timeout".to_string(),
            duration_ms: 99,
        },
        workflow_event::Event::StageCompleted {
            node_id: "agent".to_string(),
            name: "agent".to_string(),
            index: 0,
            timing: fabro_types::StageTiming::wall_only(1),
            status: "success".to_string(),
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
            attempt: 1,
            max_attempts: 1,
        },
        workflow_event::Event::StageFailed {
            node_id:    "agent".to_string(),
            name:       "agent".to_string(),
            index:      0,
            failure:    FailureDetail::new("failed", FailureCategory::Deterministic),
            will_retry: false,
            timing:     fabro_types::StageTiming::wall_only(1),
            billing:    None,
            actor:      None,
        },
    ];

    for terminal_event in terminal_events {
        let state = test_app_state();
        let app = crate::test_support::build_test_router(Arc::clone(&state));
        let run_id = fixtures::RUN_1;
        let (control_tx, _control_rx) = tokio::sync::mpsc::channel(1);
        let _temp_dir = insert_running_control_run(
            &state,
            run_id,
            Some(RunAnswerTransport::Subprocess { control_tx }),
        );
        let started = acp_event_for_stage(&run_id, &workflow_event::Event::AgentAcpStarted {
            node_id:     "agent".to_string(),
            visit:       1,
            command:     "python fake_agent.py".to_string(),
            config_name: None,
        });
        update_live_run_from_event(&state, run_id, &started);
        let activated =
            workflow_event::to_run_event(&run_id, &workflow_event::Event::AgentSessionActivated {
                node_id:          "agent".to_string(),
                visit:            1,
                session_id:       "acp-session".to_string(),
                thread_id:        None,
                provider:         Some(AgentBackend::Acp.to_string()),
                model:            None,
                reasoning_effort: None,
                speed:            None,
                permission_level: None,
                capabilities:     vec![SessionCapability::Steer],
            });
        update_live_run_from_event(&state, run_id, &activated);
        let terminal = acp_event_for_stage(&run_id, &terminal_event);
        update_live_run_from_event(&state, run_id, &terminal);

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/interrupt")))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["errors"][0]["code"], "no_active_steerable_session");
    }
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

#[tokio::test]
async fn render_graph_from_manifest_accepts_fabro_dotted_attributes() {
    let app = test_app_with();
    let dot_source = r#"digraph X {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  a [label="A", acp.command="codex"]
  start -> a -> exit
}"#;

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
                            "source": dot_source,
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
    assert_eq!(run_json_id(&items[0]).unwrap(), run_id.to_string());
    assert!(items[0]["goal"].is_string());
    assert!(items[0]["title"].is_string());
    assert!(items[0]["repository"]["name"].is_string());
    assert!(items[0]["timestamps"]["created_at"].is_string());
    assert!(run_json_status(&items[0]).is_object());
    assert!(items[0]["labels"].is_object());
    assert!(run_json_pending_control(&items[0]).is_null());
    assert!(items[0]["billing"].is_null());
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
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
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
    assert!(run_json_archived(&archive_body));
    assert_eq!(run_json_status(&archive_body)["kind"], "succeeded");
    assert_eq!(run_json_status(&archive_body)["reason"], "completed");

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
            .any(|item| run_json_id(item) == Some(&run_id.to_string())),
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
        .find(|item| run_json_id(item) == Some(&run_id.to_string()))
        .expect("archived run should appear when include_archived=true");
    assert!(run_json_archived(archived_item));
    assert_eq!(run_json_status(archived_item)["kind"], "succeeded");
    assert_eq!(run_json_status(archived_item)["reason"], "completed");

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
    assert!(!run_json_archived(&unarchive_body));
    assert_eq!(run_json_status(&unarchive_body)["kind"], "succeeded");
    assert_eq!(run_json_status(&unarchive_body)["reason"], "completed");

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
        .find(|item| run_json_id(item) == Some(&run_id.to_string()))
        .expect("unarchived run should reappear in default listing");
    assert_eq!(run_json_status(restored_item)["kind"], "succeeded");
    assert_eq!(run_json_status(restored_item)["reason"], "completed");
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
async fn delete_run_force_removes_unreadable_durable_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_unreadable_durable_run(&state, run_id).await;

    let req = Request::builder()
        .method("GET")
        .uri(api("/system/repair/runs"))
        .body(Body::empty())
        .unwrap();
    let body = response_json!(app.clone().oneshot(req).await.unwrap(), StatusCode::OK).await;
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["runs"][0]["run_id"], run_id.to_string());

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}?force=true")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NO_CONTENT).await;

    let req = Request::builder()
        .method("GET")
        .uri(api("/system/repair/runs"))
        .body(Body::empty())
        .unwrap();
    let body = response_json!(app.oneshot(req).await.unwrap(), StatusCode::OK).await;
    assert_eq!(body["total_count"], 0);
    assert!(body["runs"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn delete_run_without_force_keeps_active_durable_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    create_unreadable_durable_run(&state, run_id).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    response_json!(response, StatusCode::CONFLICT).await;

    let req = Request::builder()
        .method("GET")
        .uri(api("/system/repair/runs"))
        .body(Body::empty())
        .unwrap();
    let body = response_json!(app.oneshot(req).await.unwrap(), StatusCode::OK).await;
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["runs"][0]["run_id"], run_id.to_string());
}

#[tokio::test]
async fn delete_run_with_preserved_sandbox_returns_handoff() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let mut settings = fabro_types::WorkflowSettings::default();
    settings.run.environment.lifecycle.preserve = true;
    let graph = Graph::new("test");

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunCreated {
            run_id,
            title: None,
            settings: serde_json::to_value(settings).unwrap(),
            graph: serde_json::to_value(graph).unwrap(),
            workflow_source: None,
            workflow_config: None,
            labels: std::collections::BTreeMap::default(),
            run_dir: "/tmp/fabro-run".to_string(),
            source_directory: Some("/tmp/fabro-run".to_string()),
            workflow_slug: Some("test".to_string()),
            db_prefix: None,
            provenance: None,
            manifest_blob: None,
            git: None,
            fork_source_ref: None,
            retried_from: None,
            parent_id: None,
            web_url: None,
        },
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::SandboxInitialized {
            provider:          SandboxProvider::Local,
            id:                "sandbox-preserve-1".to_string(),
            working_directory: "/tmp/fabro-preserved-sandbox".to_string(),
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
            workspace_root:    None,
            repos_root:        None,
            primary_repo_path: None,
            primary_repo_link: None,
        },
    ])
    .await;

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}?force=true")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["deleted"].as_bool(), Some(true));
    assert_eq!(body["sandbox_preserved"].as_bool(), Some(true));
    assert_eq!(body["sandbox"]["provider"].as_str(), Some("local"));
    assert_eq!(body["sandbox"]["id"].as_str(), Some("sandbox-preserve-1"));
    assert!(body["sandbox"].get("identifier").is_none());

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn delete_run_retry_after_missing_provider_resource_removes_metadata() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = RunId::new();
    let graph = Graph::new("test");

    create_durable_run_with_events(&state, run_id, &[
        workflow_event::Event::RunCreated {
            run_id,
            title: None,
            settings: serde_json::to_value(fabro_types::WorkflowSettings::default()).unwrap(),
            graph: serde_json::to_value(graph).unwrap(),
            workflow_source: None,
            workflow_config: None,
            labels: std::collections::BTreeMap::default(),
            run_dir: "/tmp/fabro-run".to_string(),
            source_directory: Some("/tmp/fabro-run".to_string()),
            workflow_slug: Some("test".to_string()),
            db_prefix: None,
            provenance: None,
            manifest_blob: None,
            git: None,
            fork_source_ref: None,
            retried_from: None,
            parent_id: None,
            web_url: None,
        },
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::SandboxInitialized {
            provider:          SandboxProvider::Docker,
            id:                "missing-sandbox".to_string(),
            working_directory: "/tmp/fabro-missing-sandbox".to_string(),
            repo_cloned:       Some(false),
            clone_origin_url:  None,
            clone_branch:      None,
            workspace_root:    None,
            repos_root:        None,
            primary_repo_path: None,
            primary_repo_link: None,
        },
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    response_json!(response, StatusCode::CONFLICT).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}")))
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
    assert_eq!(
        body["totals"]["timing"]["wall_time_ms"].as_u64().unwrap(),
        0
    );
    assert!(body["totals"]["total_usd_micros"].is_null());
    assert!(body["by_model"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn get_aggregate_billing_returns_provider_model_speed_identity() {
    let state = test_app_state();
    {
        let mut agg = state
            .aggregate_billing
            .lock()
            .expect("aggregate billing lock");
        agg.total_runs = 1;
        agg.by_model.insert(
            ModelRef {
                provider: ProviderId::anthropic(),
                model_id: "claude-opus-4-6".to_string(),
                speed:    None,
            },
            ModelBillingTotals {
                stages:  1,
                billing: BilledTokenCounts {
                    input_tokens:       10,
                    output_tokens:      1,
                    total_tokens:       11,
                    reasoning_tokens:   0,
                    cache_read_tokens:  0,
                    cache_write_tokens: 0,
                    total_usd_micros:   Some(11),
                },
            },
        );
        agg.by_model.insert(
            ModelRef {
                provider: ProviderId::anthropic(),
                model_id: "claude-opus-4-6".to_string(),
                speed:    Some(Speed::Fast),
            },
            ModelBillingTotals {
                stages:  1,
                billing: BilledTokenCounts {
                    input_tokens:       20,
                    output_tokens:      2,
                    total_tokens:       22,
                    reasoning_tokens:   0,
                    cache_read_tokens:  0,
                    cache_write_tokens: 0,
                    total_usd_micros:   Some(22),
                },
            },
        );
    }
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/billing"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let by_model = body["by_model"].as_array().unwrap();

    assert_eq!(by_model.len(), 2);
    let standard = by_model
        .iter()
        .find(|entry| entry["model"]["speed"].is_null())
        .unwrap();
    let fast = by_model
        .iter()
        .find(|entry| entry["model"]["speed"] == "fast")
        .unwrap();
    assert_eq!(standard["model"]["provider"], "anthropic");
    assert_eq!(standard["model"]["model_id"], "claude-opus-4-6");
    assert_eq!(standard["billing"]["input_tokens"], 10);
    assert_eq!(fast["model"]["provider"], "anthropic");
    assert_eq!(fast["model"]["model_id"], "claude-opus-4-6");
    assert_eq!(fast["billing"]["input_tokens"], 20);
}

#[test]
fn aggregate_billing_counts_projection_rollup_usage_visits() {
    let mut accumulator = BillingAccumulator::default();
    let rollup = fabro_workflow::ProjectionBillingRollup {
        stages:             Vec::new(),
        totals:             BilledTokenCounts {
            input_tokens:       300,
            output_tokens:      30,
            total_tokens:       330,
            reasoning_tokens:   0,
            cache_read_tokens:  0,
            cache_write_tokens: 0,
            total_usd_micros:   Some(330),
        },
        by_model:           vec![
            fabro_workflow::ProjectionBillingByModel {
                model:   ModelRef {
                    provider: ProviderId::openai(),
                    model_id: "gpt-5.4".to_string(),
                    speed:    None,
                },
                stages:  1,
                billing: BilledTokenCounts {
                    input_tokens:       100,
                    output_tokens:      10,
                    total_tokens:       110,
                    reasoning_tokens:   0,
                    cache_read_tokens:  0,
                    cache_write_tokens: 0,
                    total_usd_micros:   Some(110),
                },
            },
            fabro_workflow::ProjectionBillingByModel {
                model:   ModelRef {
                    provider: ProviderId::openai(),
                    model_id: "gpt-5.4".to_string(),
                    speed:    Some(Speed::Fast),
                },
                stages:  1,
                billing: BilledTokenCounts {
                    input_tokens:       200,
                    output_tokens:      20,
                    total_tokens:       220,
                    reasoning_tokens:   0,
                    cache_read_tokens:  0,
                    cache_write_tokens: 0,
                    total_usd_micros:   Some(220),
                },
            },
        ],
        timing:             fabro_types::RunTiming::wall_only(2000),
        billed_visit_count: 2,
    };

    accumulate_billing_rollup(&mut accumulator, &rollup);

    assert_eq!(accumulator.total_runs, 1);
    assert_eq!(accumulator.total_timing.wall_time_ms, 2000);
    assert_eq!(accumulator.by_model.len(), 2);
    assert_eq!(
        accumulator.by_model[&ModelRef {
            provider: ProviderId::openai(),
            model_id: "gpt-5.4".to_string(),
            speed:    None,
        }]
            .stages,
        1
    );
    assert_eq!(
        accumulator.by_model[&ModelRef {
            provider: ProviderId::openai(),
            model_id: "gpt-5.4".to_string(),
            speed:    None,
        }]
            .billing
            .input_tokens,
        100
    );
    assert_eq!(
        accumulator.by_model[&ModelRef {
            provider: ProviderId::openai(),
            model_id: "gpt-5.4".to_string(),
            speed:    Some(Speed::Fast),
        }]
            .stages,
        1
    );
    assert_eq!(
        accumulator.by_model[&ModelRef {
            provider: ProviderId::openai(),
            model_id: "gpt-5.4".to_string(),
            speed:    Some(Speed::Fast),
        }]
            .billing
            .input_tokens,
        200
    );
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
    assert_eq!(run_json_status(&body)["kind"], "submitted");
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

[run.environment]
id = "local"

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
        .spec;
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
async fn cancel_runnable_run_succeeds() {
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
    assert_eq!(run_json_status(&body)["kind"], "failed");
    assert_eq!(run_json_status(&body)["reason"], "cancelled");

    // Cancelled runs appear in the runs list with a "failed" status
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id_str = run_id.to_string();
    let list_item = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| run_json_id(item) == Some(run_id_str.as_str()));
    assert!(
        list_item.is_some(),
        "cancelled run should appear in the list"
    );
    assert_eq!(
        run_json_status(list_item.unwrap())["kind"].as_str(),
        Some("failed"),
        "cancelled run should preserve the failed lifecycle status"
    );

    let run_store = state.store.open_run_reader(&run_id).await.unwrap();
    let status = run_store.state().await.unwrap().status;
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
    assert_eq!(run_json_pending_control(&body).as_str(), Some("cancel"));

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(
        summary.lifecycle.pending_control,
        Some(RunControlAction::Cancel)
    );
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
    assert_eq!(
        summary.lifecycle.pending_control,
        Some(RunControlAction::Cancel)
    );
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
    assert_eq!(run_json_status(&body)["kind"], "runnable");
    assert_eq!(run_json_pending_control(&body).as_str(), Some("pause"));

    // Verify pending_control via /runs/{id} (board no longer includes this field)
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    assert_eq!(run_json_pending_control(&body).as_str(), Some("pause"));

    // Verify the run appears in the runs list with runnable status.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let item = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| run_json_id(item) == Some(run_id_str.as_str()))
        .expect("board item should exist");
    assert!(run_json_status(item).is_object());
    assert_eq!(run_json_pending_control(item).as_str(), Some("pause"));
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
    assert_eq!(run_json_status(&body)["kind"], "paused");
    assert_eq!(
        run_json_status(&body)["prior_block"],
        "human_input_required"
    );
    assert_eq!(run_json_pending_control(&body), &serde_json::Value::Null);

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(summary.lifecycle.status, RunStatus::Paused {
        prior_block: Some(BlockedReason::HumanInputRequired),
    });
    assert_eq!(summary.lifecycle.pending_control, None);
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
    assert_eq!(run_json_status(&body)["kind"], "runnable");
    assert_eq!(run_json_pending_control(&body).as_str(), Some("unpause"));

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(
        summary.lifecycle.pending_control,
        Some(RunControlAction::Unpause)
    );
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
    assert_eq!(run_json_status(&body)["kind"], "blocked");
    assert_eq!(
        run_json_status(&body)["blocked_reason"],
        "human_input_required"
    );
    assert_eq!(run_json_pending_control(&body), &serde_json::Value::Null);

    let summary = state.store.runs().find(&run_id).await.unwrap().unwrap();
    assert_eq!(summary.lifecycle.status, RunStatus::Blocked {
        blocked_reason: BlockedReason::HumanInputRequired,
    });
    assert_eq!(summary.lifecycle.pending_control, None);
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
    assert_eq!(run_1.status, RunStatus::Submitted);

    let run_2 = state
        .store
        .open_run_reader(&fixtures::RUN_2)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    let run_2_status = run_2.status;
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
    let run_3_status = run_3.status;
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
    let run_status = run_state.status;
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

[run.environment]
id = "local"
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
                RunStatus::Runnable
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
                RunStatus::Runnable
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
        let record = run_store.state().await.unwrap().status;
        if record
            == (RunStatus::Failed {
                reason: FailureReason::Cancelled,
            })
        {
            status_record = Some(record);
            break;
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
async fn queue_position_reported_for_runnable_runs() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Create and start two runs (no scheduler, both stay runnable)
    let first_run_id = create_and_start_run(&app, MINIMAL_DOT).await;
    let second_run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    // Queue position is tracked in memory even when runnable runs are also
    // visible on the board.
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

    // With max_concurrent_runs=1, at most one run should be live "running".
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let items = body["data"].as_array().unwrap();
    let active_count = items
        .iter()
        .filter(|item| run_json_status(item)["kind"].as_str() == Some("running"))
        .count();
    assert!(
        active_count <= 1,
        "expected at most 1 active run, got {active_count}"
    );
}

#[tokio::test]
async fn submit_answer_to_unstarted_run_returns_conflict() {
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

    // Try to submit an answer to a run with no active worker.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/questions/q1/answer")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"kind": "yes"})).unwrap(),
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
async fn create_completion_unknown_provider_returns_clear_error() {
    let app = test_app_with();

    let req = Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "provider": "missing-provider",
                "model": "gpt-5.4",
                "stream": false,
                "messages": [
                    {
                        "role": "user",
                        "content": [{"kind": "text", "data": "hi"}]
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(
        body["errors"][0]["detail"],
        "Provider \"missing-provider\" is not configured"
    );
}

#[tokio::test]
async fn create_completion_default_model_uses_app_state_catalog() {
    let llm_catalog_settings: LlmCatalogSettings = toml::from_str(
        r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"
priority = 120

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
    )
    .expect("catalog fixture should parse");
    let state = TestAppStateBuilder::new()
        .llm_catalog_settings(llm_catalog_settings)
        .build();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "stream": false,
                "messages": [
                    {
                        "role": "user",
                        "content": [{"kind": "text", "data": "hi"}]
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::BAD_GATEWAY).await;
    assert!(
        body["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("Provider 'acme' not registered"),
        "unexpected error body: {body:?}"
    );
}

#[tokio::test]
async fn demo_list_runs_returns_run_list_items() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    assert!(!data.is_empty(), "demo should return runs");
    let first = &data[0];
    assert!(first["id"].is_string());
    assert!(first["goal"].is_string());
    assert!(first["repository"].is_object());
    assert!(first["title"].is_string());
    assert!(run_json_status(first).is_object());
    assert!(first["workflow"]["slug"].is_string() || first["workflow"]["slug"].is_null());
    assert!(first["labels"].is_object());
    assert!(first["timestamps"]["created_at"].is_string());
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
    // Should have Run fields, not RunStatusResponse fields
    assert!(body["id"].is_string(), "should have id field");
    assert!(body["goal"].is_string(), "should have goal field");
    assert!(
        body["workflow"]["slug"].is_string(),
        "should have workflow.slug field"
    );
    assert!(body["lifecycle"]["queue_position"].is_null());
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
async fn demo_workflows_return_list_detail_and_runs() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(state);

    let list_req = Request::builder()
        .method("GET")
        .uri(api("/workflows"))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let list_response = app.clone().oneshot(list_req).await.unwrap();
    let list_body = response_json!(list_response, StatusCode::OK).await;
    let workflows = list_body["data"]
        .as_array()
        .expect("workflow list data should be an array");
    assert!(!workflows.is_empty(), "demo should return workflows");
    let first = &workflows[0];
    assert!(first["name"].is_string());
    assert!(first["slug"].is_string());
    assert!(first["filename"].is_string());
    assert!(first["last_run"].is_object() || first["last_run"].is_null());
    assert!(first["schedule"].is_object() || first["schedule"].is_null());

    let detail_req = Request::builder()
        .method("GET")
        .uri(api("/workflows/implement"))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let detail_response = app.clone().oneshot(detail_req).await.unwrap();
    let detail_body = response_json!(detail_response, StatusCode::OK).await;
    assert_eq!(detail_body["slug"], "implement");
    assert!(detail_body["settings"].is_object());
    assert!(
        detail_body["graph"]
            .as_str()
            .is_some_and(|graph| graph.contains("digraph"))
    );

    let runs_req = Request::builder()
        .method("GET")
        .uri(api("/workflows/implement/runs"))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let runs_response = app.oneshot(runs_req).await.unwrap();
    let runs_body = response_json!(runs_response, StatusCode::OK).await;
    let runs = runs_body["data"]
        .as_array()
        .expect("workflow runs data should be an array");
    assert!(
        runs.iter()
            .all(|run| run["workflow"]["slug"].as_str() == Some("implement")),
        "workflow run list should be scoped to the requested workflow"
    );
}

#[tokio::test]
async fn list_runs_returns_run_list_items() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    {
        let id = run_id.parse::<RunId>().unwrap();
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&id).expect("run should exist");
        managed_run.status = RunStatus::Running;
    }

    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    let item = data
        .iter()
        .find(|i| run_json_id(i) == Some(&run_id))
        .expect("run should be in list");
    assert!(item["goal"].is_string());
    assert!(item["title"].is_string());
    assert!(item["repository"].is_object());
    assert!(item["workflow"]["slug"].is_string() || item["workflow"]["slug"].is_null());
    assert!(item["workflow"]["name"].is_string() || item["workflow"]["name"].is_null());
    assert!(item["workflow"]["graph_name"].is_string());
    assert!(item["labels"].is_object());
    assert!(run_json_status(item).is_object());
    assert!(item["timestamps"]["created_at"].is_string());
    assert!(run_json_pending_control(item).is_null());
    assert!(item["billing"].is_null());
}

#[tokio::test]
async fn list_runs_excludes_removing_status_by_default() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;

    // A run in Removing status should not appear by default
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
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    assert!(
        !data
            .iter()
            .any(|i| run_json_id(i) == Some(&run_id.to_string())),
        "removing run should not appear by default"
    );

    // ?status=removing opts the bucket in.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?status=removing"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    assert!(
        data.iter()
            .any(|i| run_json_id(i) == Some(&run_id.to_string())),
        "?status=removing should opt removing runs in"
    );
}

#[tokio::test]
async fn list_runs_excludes_archived_by_default() {
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
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
        workflow_event::Event::RunArchived { actor: None },
    ])
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    assert!(
        !data
            .iter()
            .any(|i| run_json_id(i) == Some(&run_id.to_string())),
        "archived run should be hidden when include_archived is unset",
    );
}

#[tokio::test]
async fn list_runs_includes_archived_when_flag_set() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let archived_id = fixtures::RUN_1;
    let succeeded_id = fixtures::RUN_2;

    create_durable_run_with_events(&state, archived_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
        workflow_event::Event::RunArchived { actor: None },
    ])
    .await;
    create_durable_run_with_events(&state, succeeded_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?include_archived=true"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");

    let archived_item = data
        .iter()
        .find(|i| run_json_id(i) == Some(&archived_id.to_string()))
        .expect("archived run should appear when include_archived=true");
    assert!(run_json_archived(archived_item));
    assert_eq!(
        run_json_status(archived_item)["kind"].as_str().unwrap(),
        "succeeded"
    );

    let succeeded_item = data
        .iter()
        .find(|i| run_json_id(i) == Some(&succeeded_id.to_string()))
        .expect("non-archived run should still appear");
    assert_eq!(
        run_json_status(succeeded_item)["kind"].as_str().unwrap(),
        "succeeded"
    );
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
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
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
        assert_eq!(
            run_json_status(&body)["kind"].as_str(),
            Some(expected_status)
        );
    }
}

#[tokio::test]
async fn list_runs_preserves_underlying_run_status_payloads() {
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
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
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
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let data = body["data"].as_array().expect("data should be array");

    let paused_item = data
        .iter()
        .find(|i| run_json_id(i) == Some(&paused_id.to_string()))
        .expect("paused run should be on board");
    assert_eq!(
        run_json_status(paused_item)["kind"].as_str().unwrap(),
        "paused"
    );
    assert!(run_json_status(paused_item)["prior_block"].is_null());

    let succeeded_item = data
        .iter()
        .find(|i| run_json_id(i) == Some(&succeeded_id.to_string()))
        .expect("succeeded run should be on board");
    assert_eq!(
        run_json_status(succeeded_item)["kind"].as_str().unwrap(),
        "succeeded"
    );
    assert_eq!(
        run_json_status(succeeded_item)["reason"].as_str().unwrap(),
        "completed"
    );

    let blocked_item = data
        .iter()
        .find(|i| run_json_id(i) == Some(&blocked_id.to_string()))
        .expect("blocked run should be on board");
    assert_eq!(
        run_json_status(blocked_item)["kind"].as_str().unwrap(),
        "blocked"
    );
    assert_eq!(
        run_json_status(blocked_item)["blocked_reason"]
            .as_str()
            .unwrap(),
        "human_input_required"
    );
    assert!(
        blocked_item["current_question"].is_object(),
        "blocked item should include the current question"
    );
}

#[tokio::test]
async fn list_runs_includes_live_metadata_from_run_state() {
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
            provider:          SandboxProvider::Local,
            id:                "sb-test".to_string(),
            working_directory: "/sandbox/workdir".to_string(),
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
            workspace_root:    None,
            repos_root:        None,
            primary_repo_path: None,
            primary_repo_link: None,
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
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let data = body["data"].as_array().expect("data should be array");
    let item = data
        .iter()
        .find(|i| run_json_id(i) == Some(&run_id.to_string()))
        .expect("run should be in board");

    assert_eq!(item["pull_request"]["number"].as_u64(), Some(42));
    assert_eq!(item["sandbox"]["runtime"]["id"].as_str(), Some("sb-test"));
    assert_eq!(
        item["sandbox"]["runtime"]["working_directory"].as_str(),
        Some("/sandbox/workdir")
    );
    assert!(item["current_question"].is_object());
}

#[tokio::test]
async fn list_runs_page_limit_preserves_metadata_for_paged_items() {
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
                provider:          SandboxProvider::Local,
                id:                sandbox_id.to_string(),
                working_directory: "/sandbox/workdir".to_string(),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
                workspace_root:    None,
                repos_root:        None,
                primary_repo_path: None,
                primary_repo_link: None,
            },
        ] {
            workflow_event::append_event(&run_store, &run_id, &event)
                .await
                .unwrap();
        }
    }

    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?page[limit]=1"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["meta"]["has_more"].as_bool(), Some(true));

    let data = body["data"].as_array().expect("data should be array");
    assert_eq!(data.len(), 1);

    let item = &data[0];
    let sandbox_id = item["sandbox"]["runtime"]["id"]
        .as_str()
        .expect("paged item should still include sandbox metadata");
    assert!(matches!(sandbox_id, "sb-first" | "sb-second"));
}

#[tokio::test]
async fn list_runs_status_filter_accepts_repeated_values() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // Running run (will map to BoardColumn::Running)
    let running_id = fixtures::RUN_1;
    create_durable_run_with_events(&state, running_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    // Succeeded run (BoardColumn::Succeeded)
    let succeeded_id = fixtures::RUN_2;
    create_durable_run_with_events(&state, succeeded_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    // Pending run (BoardColumn::Pending via Submitted)
    let pending_id = fixtures::RUN_3;
    create_durable_run_with_events(&state, pending_id, &[workflow_event::Event::RunSubmitted {
        definition_blob: None,
    }])
    .await;

    // Single value: only running.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?status=running"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(run_json_id)
        .collect();
    assert!(ids.contains(&running_id.to_string().as_str()));
    assert!(!ids.contains(&succeeded_id.to_string().as_str()));
    assert!(!ids.contains(&pending_id.to_string().as_str()));

    // Repeated values: running + succeeded.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?status=running&status=succeeded"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(run_json_id)
        .collect();
    assert!(ids.contains(&running_id.to_string().as_str()));
    assert!(ids.contains(&succeeded_id.to_string().as_str()));
    assert!(!ids.contains(&pending_id.to_string().as_str()));
}

#[tokio::test]
async fn list_runs_sort_direction_reverses_order_with_stable_tiebreak() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // All fixtures share timestamp=0; the id-desc tiebreak controls order.
    let ids = [fixtures::RUN_1, fixtures::RUN_2, fixtures::RUN_3];
    for id in &ids {
        create_durable_run_with_events(&state, *id, &[
            workflow_event::Event::RunSubmitted {
                definition_blob: None,
            },
            workflow_event::Event::RunStarting,
            workflow_event::Event::RunRunning,
        ])
        .await;
    }

    // Default (sort=created_at desc): tiebreak puts higher ids first.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let observed: Vec<String> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(run_json_id)
        .map(str::to_string)
        .collect();
    let mut expected: Vec<String> = ids.iter().map(std::string::ToString::to_string).collect();
    expected.sort_by(|a, b| b.cmp(a)); // desc by id
    assert_eq!(observed, expected, "default desc order with id tiebreak");

    // Ascending: timestamps still tie, then id desc tiebreak still applies.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?sort=created_at&direction=asc"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let observed: Vec<String> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(run_json_id)
        .map(str::to_string)
        .collect();
    assert_eq!(
        observed, expected,
        "asc still uses id-desc tiebreak for tied keys"
    );
}

#[tokio::test]
async fn list_runs_sort_by_status_groups_by_bucket() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // BoardColumn enum order: pending < runnable < initializing < running <
    // blocked < succeeded < failed < archived < removing. Use three distinct
    // buckets.
    let pending_id = fixtures::RUN_1;
    create_durable_run_with_events(&state, pending_id, &[workflow_event::Event::RunSubmitted {
        definition_blob: None,
    }])
    .await;

    let succeeded_id = fixtures::RUN_2;
    create_durable_run_with_events(&state, succeeded_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
        workflow_event::Event::WorkflowRunCompleted {
            timing:               fabro_types::RunTiming::wall_only(1000),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        },
    ])
    .await;

    let running_id = fixtures::RUN_3;
    create_durable_run_with_events(&state, running_id, &[
        workflow_event::Event::RunSubmitted {
            definition_blob: None,
        },
        workflow_event::Event::RunStarting,
        workflow_event::Event::RunRunning,
    ])
    .await;

    // sort=status asc: pending < running < succeeded.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?sort=status&direction=asc"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let observed: Vec<String> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(run_json_id)
        .map(str::to_string)
        .collect();
    assert_eq!(observed, vec![
        pending_id.to_string(),
        running_id.to_string(),
        succeeded_id.to_string(),
    ]);
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
            EventBody::RunRunnable(fabro_types::run_event::RunRunnableProps {
                source: fabro_types::RunRunnableSource::StartRequested,
            }),
        ))
        .unwrap();
    event_tx
        .send(test_event_envelope(
            2,
            run_one,
            EventBody::RunRunnable(fabro_types::run_event::RunRunnableProps {
                source: fabro_types::RunRunnableSource::StartRequested,
            }),
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
